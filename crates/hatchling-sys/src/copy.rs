//! Reading, and copying, a directory tree through handles (WO-029).
//!
//! What the broker copies here, a browser's bundle, is the operator's to
//! name and the broker's to run: it must reach the cell exactly as it was
//! checked. So nothing here resolves a path twice:
//!
//! - each directory is opened from its parent's handle, by a name its
//!   parent listed, never following a symlink: the walk can't leave the
//!   tree, and a symlink anywhere in it is refused, not followed;
//! - a file is read through the handle that was looked at, and what's
//!   reported for it is the bytes as they're written to the copy: a file
//!   that changes while it's copied is reported as copied;
//! - anything but a regular file or a directory is refused;
//! - the walk is bounded in entries, bytes and depth, so a tree that grows
//!   while it's walked can't hold it for ever;
//! - names are visited in order and must be UTF-8 without a newline, so
//!   what's reported describes the tree the same way every time.
//!
//! A copied file is read-only, `0555` if any execute bit was set and
//! `0444` otherwise; a copied directory is `0755`, so its owner can remove
//! the copy.

use std::ffi::{CString, OsStr};
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use rustix::fs::{FileType, Mode, OFlags};

use crate::Error;

/// How a directory, or a file, in the source is opened: itself, never a
/// symlink's target, and never waiting (a FIFO is refused, not read).
const SOURCE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

/// How a directory of the copy is opened.
const TARGET_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// How a file of the copy is made: new, and never through a symlink.
const TARGET_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// One piece of a file read at a time.
const PIECE: usize = 64 * 1024;

/// How much of a tree a walk takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLimits {
    /// Most entries, files and directories.
    pub entries: usize,
    /// Most bytes, all files together.
    pub bytes: u64,
    /// How deep a directory may be: the tree's own entries are at 1.
    pub depth: usize,
}

/// What a walk reports, in order: each directory; each file, then its
/// bytes as they're read (and written), then its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeEntry<'a> {
    /// A directory, by its path in the tree.
    Dir {
        /// Its path, `/`-separated, relative to the tree.
        path: &'a str,
    },
    /// A file begins.
    File {
        /// Its path, `/`-separated, relative to the tree.
        path: &'a str,
        /// Whether any of its execute bits is set.
        executable: bool,
    },
    /// A piece of the file begun last, as read.
    Bytes(&'a [u8]),
    /// The file begun last ends, after `size` bytes.
    End {
        /// How many bytes it had.
        size: u64,
    },
}

/// Walks the tree at `source`, reporting each entry to `on`, and copies it
/// into the empty directory `target` when one is given.
///
/// The walk follows no symlink: `source` itself is opened as a directory,
/// not through a symlink in its last part.
///
/// # Errors
///
/// [`Error::Copy`] naming the path: a symlink, anything but a file or a
/// directory, a name that isn't UTF-8 or holds a newline, past `limits`,
/// or an operation that failed.
pub fn copy_tree(
    source: &Path,
    target: Option<&Path>,
    limits: TreeLimits,
    on: &mut dyn FnMut(TreeEntry<'_>),
) -> Result<(), Error> {
    let source_root = rustix::fs::open(source, SOURCE_FLAGS | OFlags::DIRECTORY, Mode::empty())
        .map_err(failed("open the tree", ""))?;
    let target_root = target
        .map(|target| rustix::fs::open(target, TARGET_DIR_FLAGS, Mode::empty()))
        .transpose()
        .map_err(failed("open the copy", ""))?;
    let mut walk = Walk {
        limits,
        entries: 0,
        bytes: 0,
        on,
    };
    walk.dir(&source_root, target_root.as_ref(), "", 1)
}

/// A walk in progress: what it has taken, and where it reports.
struct Walk<'o> {
    limits: TreeLimits,
    entries: usize,
    bytes: u64,
    on: &'o mut dyn FnMut(TreeEntry<'_>),
}

impl Walk<'_> {
    /// Walks the directory open at `source`, at `prefix` and `depth`, into
    /// `target` when there's a copy.
    fn dir(
        &mut self,
        source: &OwnedFd,
        target: Option<&OwnedFd>,
        prefix: &str,
        depth: usize,
    ) -> Result<(), Error> {
        if depth > self.limits.depth {
            return Err(refused(prefix, "the tree is deeper than its bound"));
        }
        for name in names(source, prefix)? {
            self.entries += 1;
            if self.entries > self.limits.entries {
                return Err(refused(prefix, "the tree has more entries than its bound"));
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let c_name =
                CString::new(name.as_bytes()).map_err(|_| refused(&path, "a name holds a NUL"))?;
            let opened = rustix::fs::openat(source, &c_name, SOURCE_FLAGS, Mode::empty()).map_err(
                |errno| {
                    if errno == rustix::io::Errno::LOOP {
                        refused(&path, "a symlink: it isn't followed")
                    } else {
                        failed("open an entry", &path)(errno)
                    }
                },
            )?;
            let stat = rustix::fs::fstat(&opened).map_err(failed("look at an entry", &path))?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    (self.on)(TreeEntry::Dir { path: &path });
                    let copy = target
                        .map(|target| {
                            rustix::fs::mkdirat(target, &c_name, Mode::from_raw_mode(0o755))
                                .and_then(|()| {
                                    rustix::fs::openat(
                                        target,
                                        &c_name,
                                        TARGET_DIR_FLAGS,
                                        Mode::empty(),
                                    )
                                })
                        })
                        .transpose()
                        .map_err(failed("make a directory of the copy", &path))?;
                    self.dir(&opened, copy.as_ref(), &path, depth + 1)?;
                }
                FileType::RegularFile => {
                    let executable = stat.st_mode & 0o111 != 0;
                    (self.on)(TreeEntry::File {
                        path: &path,
                        executable,
                    });
                    // Read-only from the start: the handle, opened for
                    // writing, keeps the right to write.
                    let mode = if executable { 0o555 } else { 0o444 };
                    let copy = target
                        .map(|target| {
                            rustix::fs::openat(
                                target,
                                &c_name,
                                TARGET_FILE_FLAGS,
                                Mode::from_raw_mode(0o600),
                            )
                            .and_then(|copy| {
                                rustix::fs::fchmod(&copy, Mode::from_raw_mode(mode)).map(|()| copy)
                            })
                        })
                        .transpose()
                        .map_err(failed("make a file of the copy", &path))?;
                    let size = self.file(opened, copy, &path)?;
                    (self.on)(TreeEntry::End { size });
                }
                FileType::Symlink
                | FileType::Fifo
                | FileType::Socket
                | FileType::CharacterDevice
                | FileType::BlockDevice
                | FileType::Unknown => {
                    return Err(refused(&path, "neither a file nor a directory"));
                }
            }
        }
        Ok(())
    }

    /// Reads the file open at `source` within the byte bound, writing each
    /// piece to `target` when there's a copy and then reporting it: how
    /// many bytes it had.
    fn file(&mut self, source: OwnedFd, target: Option<OwnedFd>, path: &str) -> Result<u64, Error> {
        let mut reader = std::fs::File::from(source);
        let mut writer = target.map(std::fs::File::from);
        let mut piece = vec![0_u8; PIECE];
        let mut size = 0_u64;
        loop {
            let read = match reader.read(&mut piece) {
                Ok(0) => return Ok(size),
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(copy_error("read a file", path, error)),
            };
            let read_bytes = u64::try_from(read).unwrap_or(u64::MAX);
            self.bytes = self.bytes.saturating_add(read_bytes);
            if self.bytes > self.limits.bytes {
                return Err(refused(path, "the tree has more bytes than its bound"));
            }
            let bytes = piece.get(..read).unwrap_or_default();
            if let Some(writer) = writer.as_mut() {
                writer
                    .write_all(bytes)
                    .map_err(|error| copy_error("write a file of the copy", path, error))?;
            }
            size += read_bytes;
            (self.on)(TreeEntry::Bytes(bytes));
        }
    }
}

/// The names `dir` lists, `.` and `..` aside, in order: each UTF-8, with
/// no newline.
fn names(dir: &OwnedFd, prefix: &str) -> Result<Vec<String>, Error> {
    let entries = rustix::fs::Dir::read_from(dir).map_err(failed("read a directory", prefix))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(failed("read a directory", prefix))?;
        let raw = OsStr::from_bytes(entry.file_name().to_bytes());
        if raw == "." || raw == ".." {
            continue;
        }
        let name = raw
            .to_str()
            .filter(|name| !name.contains('\n'))
            .ok_or_else(|| refused(prefix, "a name that isn't UTF-8, or holds a newline"))?;
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

/// What a failed `operation` on `path` is.
fn failed<'p>(operation: &'static str, path: &'p str) -> impl Fn(rustix::io::Errno) -> Error + 'p {
    move |errno| {
        copy_error(
            operation,
            path,
            io::Error::from_raw_os_error(errno.raw_os_error()),
        )
    }
}

fn copy_error(operation: &'static str, path: &str, source: io::Error) -> Error {
    Error::Copy {
        operation,
        path: path.to_owned(),
        source,
    }
}

/// `path` refused, for `reason`.
fn refused(path: &str, reason: &'static str) -> Error {
    copy_error("walk the tree", path, io::Error::other(reason))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use super::*;

    const ROOMY: TreeLimits = TreeLimits {
        entries: 100,
        bytes: 1 << 20,
        depth: 8,
    };

    /// A fresh directory for `name`, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("hatchling-copy-{}-{name}", std::process::id()));
            let _ = crate::remove_dir_tree(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = crate::remove_dir_tree(&self.0);
        }
    }

    /// A small bundle: an executable, a data file, and a directory of one.
    fn bundle(root: &Path) -> PathBuf {
        let source = root.join("source");
        std::fs::create_dir_all(source.join("locales")).unwrap();
        std::fs::write(source.join("chrome"), b"#!/bin/true\n").unwrap();
        std::fs::set_permissions(
            source.join("chrome"),
            std::fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        std::fs::write(source.join("icudtl.dat"), vec![7_u8; PIECE + 5]).unwrap();
        std::fs::write(source.join("locales/en.pak"), b"en").unwrap();
        source
    }

    /// What a walk reported, as lines: `d path`, `f path x`, `n bytes`,
    /// `e size`.
    fn walked(
        source: &Path,
        target: Option<&Path>,
        limits: TreeLimits,
    ) -> Result<Vec<String>, Error> {
        let mut seen = Vec::new();
        copy_tree(source, target, limits, &mut |entry| {
            seen.push(match entry {
                TreeEntry::Dir { path } => format!("d {path}"),
                TreeEntry::File { path, executable } => format!("f {path} {executable}"),
                TreeEntry::Bytes(bytes) => format!("n {}", bytes.len()),
                TreeEntry::End { size } => format!("e {size}"),
            });
        })?;
        Ok(seen)
    }

    #[test]
    fn a_tree_is_walked_in_order_and_copied_read_only() {
        let scratch = Scratch::new("copy");
        let source = bundle(&scratch.0);
        let target = scratch.0.join("target");
        std::fs::create_dir(&target).unwrap();
        let seen = walked(&source, Some(&target), ROOMY).unwrap();
        assert_eq!(
            seen,
            [
                "f chrome true",
                "n 12",
                "e 12",
                "f icudtl.dat false",
                &format!("n {PIECE}"),
                "n 5",
                &format!("e {}", PIECE + 5),
                "d locales",
                "f locales/en.pak false",
                "n 2",
                "e 2",
            ]
        );
        assert_eq!(
            std::fs::read(target.join("chrome")).unwrap(),
            b"#!/bin/true\n"
        );
        assert_eq!(std::fs::read(target.join("locales/en.pak")).unwrap(), b"en");
        assert_eq!(
            std::fs::read(target.join("icudtl.dat")).unwrap().len(),
            PIECE + 5
        );
        let mode = |path: &str| {
            std::fs::metadata(target.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("chrome"), 0o555);
        assert_eq!(mode("icudtl.dat"), 0o444);
        assert_eq!(mode("locales"), 0o755);
        // Walked alone, it reports the same and copies nothing.
        assert_eq!(walked(&source, None, ROOMY).unwrap(), seen);
    }

    #[test]
    fn a_symlink_or_a_special_file_is_refused_not_followed() {
        let scratch = Scratch::new("refuse");
        let source = bundle(&scratch.0);
        std::os::unix::fs::symlink("/etc/passwd", source.join("locales/link")).unwrap();
        let refused = walked(&source, None, ROOMY).unwrap_err().to_string();
        assert!(refused.contains("locales/link"), "{refused}");
        assert!(refused.contains("a symlink"), "{refused}");
        std::fs::remove_file(source.join("locales/link")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            source.join("pipe"),
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let refused = walked(&source, None, ROOMY).unwrap_err().to_string();
        assert!(
            refused.contains("neither a file nor a directory"),
            "{refused}"
        );
        std::fs::remove_file(source.join("pipe")).unwrap();
        // The tree itself, through a symlink: refused too.
        let link = scratch.0.join("linked");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(walked(&link, None, ROOMY).is_err());
    }

    #[test]
    fn a_walk_stops_at_its_bounds() {
        let scratch = Scratch::new("bounds");
        let source = bundle(&scratch.0);
        for (limits, says) in [
            (
                TreeLimits {
                    entries: 3,
                    ..ROOMY
                },
                "more entries",
            ),
            (
                TreeLimits {
                    bytes: u64::try_from(PIECE).unwrap(),
                    ..ROOMY
                },
                "more bytes",
            ),
            (TreeLimits { depth: 1, ..ROOMY }, "deeper"),
        ] {
            let refused = walked(&source, None, limits).unwrap_err().to_string();
            assert!(refused.contains(says), "{says}: {refused}");
        }
        // Exactly at each bound: walked.
        let exact = TreeLimits {
            entries: 4,
            bytes: u64::try_from(PIECE + 5 + 12 + 2).unwrap(),
            depth: 2,
        };
        assert!(walked(&source, None, exact).is_ok());
    }

    #[test]
    fn a_copy_lands_only_in_a_fresh_target() {
        let scratch = Scratch::new("fresh");
        let source = bundle(&scratch.0);
        let target = scratch.0.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("chrome"), b"already here").unwrap();
        let refused = walked(&source, Some(&target), ROOMY)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("make a file of the copy"), "{refused}");
        assert!(refused.contains("chrome"), "{refused}");
        let missing = walked(&source, Some(&scratch.0.join("absent")), ROOMY).unwrap_err();
        assert!(missing.to_string().contains("open the copy"), "{missing}");
    }

    #[test]
    fn a_failure_names_its_operation_and_its_entry() {
        let error = copy_error("read a file", "locales/en.pak", io::Error::other("gone"));
        assert_eq!(
            error.to_string(),
            "copy failed at \"locales/en.pak\": read a file: gone"
        );
        let Error::Copy {
            operation, path, ..
        } = refused("a/b", "why")
        else {
            unreachable!("a refusal is a copy error");
        };
        assert_eq!((operation, path.as_str()), ("walk the tree", "a/b"));
    }

    #[test]
    fn a_name_that_cant_be_described_is_refused() {
        let scratch = Scratch::new("names");
        let source = bundle(&scratch.0);
        std::fs::write(source.join("new\nline"), b"x").unwrap();
        let refused = walked(&source, None, ROOMY).unwrap_err().to_string();
        assert!(refused.contains("newline"), "{refused}");
    }
}
