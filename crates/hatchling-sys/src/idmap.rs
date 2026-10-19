//! User/group id maps for new user namespaces.
//!
//! An [`IdMap`] describes one line of `/proc/<pid>/uid_map`: in-cell
//! uids `0..count` map to host uids `base..base+count`. The kernel
//! enforces its own rules when the parent writes the map (an
//! unprivileged parent may only map its own uid); this module enforces
//! hatchling's policy *before* that:
//!
//! - `count` is at least 1 and `base + count` does not overflow `u32`.
//! - When the broker runs as root (it has a choice of base), in-cell 0
//!   must not map to host 0: the cell's root must be an unprivileged
//!   host uid, or the "unprivileged cell" is a fiction.
//! - When the broker is unprivileged, the only map the kernel will
//!   accept is its own uid, so anything else is rejected early with a
//!   clear error instead of a late `EPERM` from the map write.

use std::fmt::Write as _;

use crate::Error;

/// One id-map line: in-cell `0..count` maps to host `base..base+count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdMap {
    base: u32,
    count: u32,
}

impl IdMap {
    /// Builds a map, validating the arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdMap`] if `count` is zero or
    /// `base + count` overflows `u32`.
    pub fn new(base: u32, count: u32) -> Result<Self, Error> {
        if count == 0 {
            return Err(Error::InvalidIdMap {
                reason: String::from("id map count must be at least 1"),
            });
        }
        base.checked_add(count).ok_or_else(|| Error::InvalidIdMap {
            reason: format!("id map range {base}..{base}+{count} overflows u32"),
        })?;
        Ok(Self { base, count })
    }

    /// The host uid that in-cell uid 0 maps to.
    #[must_use]
    pub const fn base(&self) -> u32 {
        self.base
    }

    /// How many uids the map covers.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Whether `uid` falls inside this map's host range.
    ///
    /// The range end is computed with checked addition: `new` rejects
    /// an overflowing range at construction, and this keeps the check
    /// sound even if a future constructor forgets that validation.
    /// Extracted so callers never hand-roll `base + count`.
    #[must_use]
    pub const fn contains(&self, uid: u32) -> bool {
        match self.base.checked_add(self.count) {
            Some(end) => uid >= self.base && uid < end,
            None => false,
        }
    }

    /// Validates the map against the broker's own uid.
    ///
    /// `euid` is the broker process's effective uid — the uid whose
    /// perspective the kernel enforces the map write from.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdMap`] if a root broker would map
    /// in-cell 0 to host 0, or if an unprivileged broker configured
    /// anything other than its own single uid (the only map the kernel
    /// would accept).
    pub fn validate_for_euid(&self, euid: u32) -> Result<(), Error> {
        if euid == 0 {
            if self.base == 0 {
                return Err(Error::InvalidIdMap {
                    reason: String::from(
                        "a root broker must not map in-cell 0 to host 0: \
                         the cell's root must be an unprivileged host uid",
                    ),
                });
            }
            if self.count != 1 {
                return Err(Error::InvalidIdMap {
                    reason: format!(
                        "a root broker maps one uid for a cell, not {}: a wider \
                         map would let the cell act as every host account it covers",
                        self.count
                    ),
                });
            }
            return Ok(());
        }
        if self.base != euid || self.count != 1 {
            return Err(Error::InvalidIdMap {
                reason: format!(
                    "an unprivileged broker (uid {euid}) can only map its own \
                     uid: expected base={euid} count=1"
                ),
            });
        }
        Ok(())
    }

    /// Renders the `/proc/<pid>/uid_map` (or `gid_map`) line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut line = String::new();
        let _ = writeln!(line, "0 {} {}", self.base, self.count);
        line
    }
}

/// Writes the uid and gid maps for a child that just entered a new user
/// namespace.
///
/// `setgroups` is set to `deny` first: an unprivileged parent cannot
/// write `gid_map` otherwise, and for a privileged parent the deny is
/// harmless. A missing `setgroups` file (`ENOENT`, kernels without the
/// file) is tolerated — the subsequent `gid_map` write is the authority
/// on whether the map took.
///
/// # Errors
///
/// Returns [`Error::IdMapWrite`] if any map write fails.
pub fn write_maps(child_pid: i32, uid_map: &IdMap, gid_map: &IdMap) -> Result<(), Error> {
    let io_err = |source: std::io::Error| Error::IdMapWrite {
        pid: child_pid,
        source,
    };
    deny_setgroups(std::path::Path::new(&format!(
        "/proc/{child_pid}/setgroups"
    )))
    .map_err(io_err)?;
    std::fs::write(format!("/proc/{child_pid}/uid_map"), uid_map.render()).map_err(io_err)?;
    std::fs::write(format!("/proc/{child_pid}/gid_map"), gid_map.render()).map_err(io_err)?;
    Ok(())
}

/// Writes "deny" to a `setgroups` file, tolerating `NotFound`.
///
/// Extracted for testability: the [`NotFound`] tolerance is
/// security-relevant (a missing file on old kernels must not abort the
/// map write), but tolerating any other error would mask real
/// failures. The mutants on the match guard (`== NotFound` to `true`,
/// `false`, or `!=`) are killed by the tests below.
fn deny_setgroups(path: &std::path::Path) -> Result<(), std::io::Error> {
    match std::fs::write(path, "deny") {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accepts_a_sane_map() {
        let map = IdMap::new(65534, 1).unwrap();
        assert_eq!(map.base(), 65534);
        assert_eq!(map.count(), 1);
        assert_eq!(map.render(), "0 65534 1\n");
    }

    #[test]
    fn new_rejects_zero_count() {
        // Pins the `count == 0` check: without it a zero-length map
        // would render "0 <base> 0", which the kernel rejects with
        // EINVAL at write time instead of failing fast here.
        let err = IdMap::new(1000, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidIdMap { .. }));
    }

    #[test]
    fn new_rejects_overflowing_range() {
        // Boundary: base + count must not wrap u32. u32::MAX as base
        // with count 1 overflows; u32::MAX - 1 with count 1 does not.
        assert!(IdMap::new(u32::MAX, 1).is_err());
        assert!(IdMap::new(u32::MAX - 1, 1).is_ok());
    }

    #[test]
    fn root_broker_must_not_map_cell_root_to_host_root() {
        // The policy pin: a root broker mapping 0->0 would make the
        // cell's root the host's root. Unprivileged-cell means
        // unprivileged.
        let map = IdMap::new(0, 1).unwrap();
        let err = map.validate_for_euid(0).unwrap_err();
        assert!(matches!(err, Error::InvalidIdMap { .. }));
        // A non-zero base is fine for root.
        IdMap::new(65534, 1).unwrap().validate_for_euid(0).unwrap();
        // One uid only: a wider map covers other host accounts.
        for count in [2, 65_536] {
            let wide = IdMap::new(100_000, count).unwrap().validate_for_euid(0);
            assert!(
                matches!(&wide, Err(Error::InvalidIdMap { reason }) if reason.contains("one uid")),
                "{count}: {wide:?}"
            );
        }
    }

    #[test]
    fn unprivileged_broker_can_only_map_itself() {
        // The kernel only accepts a single-line self-map from an
        // unprivileged parent; anything else fails fast here with a
        // clear error instead of EPERM from the write.
        IdMap::new(1000, 1)
            .unwrap()
            .validate_for_euid(1000)
            .unwrap();
        assert!(
            IdMap::new(1001, 1)
                .unwrap()
                .validate_for_euid(1000)
                .is_err()
        );
        assert!(
            IdMap::new(1000, 2)
                .unwrap()
                .validate_for_euid(1000)
                .is_err()
        );
        assert!(IdMap::new(0, 1).unwrap().validate_for_euid(1000).is_err());
    }

    #[test]
    fn render_format_is_exact() {
        // The kernel parses "ID-inside-ns ID-outside-ns length". The
        // literal pins field order: swapping base/count would silently
        // map the wrong range.
        assert_eq!(
            IdMap::new(100_000, 65_536).unwrap().render(),
            "0 100000 65536\n"
        );
    }

    #[test]
    fn count_returns_the_configured_count() {
        // Mutant: count() replaced with constant 1. A map with
        // count != 1 must report its actual count.
        let map = IdMap::new(100_000, 65_536).unwrap();
        assert_eq!(map.count(), 65_536);
        let map = IdMap::new(1000, 5).unwrap();
        assert_eq!(map.count(), 5);
    }

    #[test]
    fn contains_checks_the_half_open_range() {
        // Mutants: `>=` to `>`, `<` to `<=`, `&&` to `||`, or the
        // checked_add dropped. The range is [base, base+count):
        // base is in, base+count is out, and both comparisons matter.
        let map = IdMap::new(1000, 10).unwrap();
        assert!(map.contains(1000), "range start is inside");
        assert!(map.contains(1005), "range middle is inside");
        assert!(map.contains(1009), "range end - 1 is inside");
        assert!(!map.contains(999), "below the range is outside");
        assert!(!map.contains(1010), "base + count is outside (half-open)");
        assert!(!map.contains(u32::MAX), "far outside is outside");
        // A single-uid map contains exactly that uid.
        let single = IdMap::new(1000, 1).unwrap();
        assert!(single.contains(1000));
        assert!(!single.contains(1001));
    }

    #[test]
    fn write_maps_writes_the_rendered_maps() {
        // Fork-based: the child unshares a user namespace, the parent
        // writes the maps, then verifies by reading them back from
        // /proc. Mutants: write_maps replaced with Ok(()), or the
        // NotFound guard inverted (which would turn a missing
        // setgroups file into a hard error, or vice versa).
        //
        // The maps are this process's own uid and gid, the only ones it
        // may map: read before the fork, as a namespace would show them
        // as the overflow ids. If user namespaces are unavailable here
        // (the child says so), the test skips, unless
        // HATCHLING_REQUIRE_CELL=1; once they are, the write must work.
        use std::io::{Read as _, Write as _};

        use crate::process::{ForkResult, exit_child, fork};

        let uid_map = IdMap::new(crate::process::euid(), 1).unwrap();
        let gid_map = IdMap::new(crate::process::egid(), 1).unwrap();
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();

        let child = match fork().expect("fork for write_maps test") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // SAFETY: unshare(CLONE_NEWUSER) creates a new user
                // namespace; the child then waits for the parent to
                // write the maps.
                let ret = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
                if ret != 0 {
                    exit_child(99);
                }
                if (&theirs).write_all(b"u").is_err() {
                    exit_child(98);
                }
                // Wait for the parent: sleep in a loop until killed.
                loop {
                    // SAFETY: libc::sleep takes a plain u32 duration and
                    // has no preconditions on Rust state; the child is
                    // single-threaded after fork here.
                    unsafe { libc::sleep(30) };
                }
            }
        };

        // The raw pid is only used for /proc paths below — never as a
        // signal target.
        let child_pid = child.pid().get();

        // Its own namespace, once it says so; nothing if it couldn't.
        drop(theirs);
        ours.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut unshared = [0_u8; 1];
        if ours.read_exact(&mut unshared).is_err() {
            drop(child);
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but no user namespace here"
            );
            return;
        }

        let write_result = write_maps(child_pid, &uid_map, &gid_map);

        // Read back the maps to verify they were written.
        let uid_content =
            std::fs::read_to_string(format!("/proc/{child_pid}/uid_map")).unwrap_or_default();
        let gid_content =
            std::fs::read_to_string(format!("/proc/{child_pid}/gid_map")).unwrap_or_default();

        // Clean up the child: kill-on-drop SIGKILLs and reaps.
        drop(child);

        write_result.expect("write_maps must succeed in a namespace of the child's own");
        // The kernel pretty-prints map lines with padded columns
        // (e.g. "         0       1000          1\n"), so compare the
        // parsed (inside, outside, length) triple rather than the
        // exact bytes.
        let parse_map = |content: &str| -> (u32, u32, u32) {
            let mut parts = content.split_whitespace();
            let inside: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let outside: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let length: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            (inside, outside, length)
        };
        assert_eq!(
            parse_map(&uid_content),
            (0, uid_map.base(), uid_map.count()),
            "uid_map content must match rendered map"
        );
        assert_eq!(
            parse_map(&gid_content),
            (0, gid_map.base(), gid_map.count()),
            "gid_map content must match rendered map"
        );
    }
}

#[cfg(test)]
mod deny_setgroups_tests {
    use super::*;

    #[test]
    fn deny_setgroups_tolerates_missing_file() {
        // Mutants: guard `== NotFound` to `false`, or `==` to `!=`.
        // A missing setgroups file (old kernels) must be tolerated —
        // the gid_map write is the authority. With the guard forced
        // false (or inverted), NotFound would propagate as Err.
        let missing = std::path::Path::new("/proc/1/definitely-not-setgroups-xyz");
        assert!(
            deny_setgroups(missing).is_ok(),
            "missing setgroups file must be tolerated"
        );
    }

    #[test]
    fn deny_setgroups_rejects_other_errors() {
        // Mutant: guard `== NotFound` to `true` (tolerate everything).
        // Writing to a directory fails with IsADirectory, not
        // NotFound — this must propagate as Err. The `true` mutant
        // would swallow it and return Ok.
        let dir = std::env::temp_dir();
        let err = deny_setgroups(&dir).expect_err("writing deny to a directory must fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::IsADirectory,
            "expected IsADirectory, got {:?}",
            err.kind()
        );
    }
}
