//! A cell's own uid without root: the owner's subordinate ranges, and the
//! setuid helpers that map them (WO-014).
//!
//! An unprivileged process may map only its own uid into a user
//! namespace, so an unprivileged broker's cells would share the owner's
//! uid, and the broker admits callers by uid. Rootless containers solve
//! this the same way: the administrator grants the owner a subordinate
//! range in `/etc/subuid` and `/etc/subgid`, and the setuid helpers
//! `newuidmap` and `newgidmap` write a map into it on the owner's behalf.
//!
//! This module reads that range and refuses an unsafe one before any
//! cell spawns:
//!
//! - the range must not hold uid 0, the owner's uid, or any account's
//!   uid (`/etc/passwd`; for gids, `/etc/group`): a cell mapped onto one
//!   would act as that account on the host;
//! - each helper must be a regular file owned by root, with its setuid
//!   bit, not a symlink: anything else could be someone else's program
//!   running with the owner's grant.
//!
//! A cell maps in-cell 0 to the range's first id, one id wide.

use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Error;

/// Where the standard `newuidmap` lives.
pub const NEWUIDMAP: &str = "/usr/bin/newuidmap";

/// Where the standard `newgidmap` lives.
pub const NEWGIDMAP: &str = "/usr/bin/newgidmap";

/// The setuid bit of a file mode.
const SETUID: u32 = 0o4000;

/// One subordinate range: ids `start..start + count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubRange {
    /// The first id.
    pub start: u32,
    /// How many ids.
    pub count: u32,
}

impl SubRange {
    /// Whether `id` is in the range.
    #[must_use]
    pub fn holds(&self, id: u32) -> bool {
        id >= self.start && u64::from(id) < u64::from(self.start) + u64::from(self.count)
    }
}

/// The first range `subid` text grants the account `name` (id `id`).
///
/// `subid` is `/etc/subuid` or `/etc/subgid`: lines `name:start:count` or
/// `id:start:count`. Comments, blank lines, malformed lines, an empty
/// range and one that runs past `u32::MAX` are skipped.
#[must_use]
pub fn range_for(subid: &str, name: &str, id: u32) -> Option<SubRange> {
    subid.lines().find_map(|line| {
        let mut fields = line.trim().split(':');
        let (Some(who), Some(start), Some(count), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if who != name && who.parse::<u32>().ok() != Some(id) {
            return None;
        }
        let (start, count) = (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?);
        let fits = count > 0 && u64::from(start) + u64::from(count) <= u64::from(u32::MAX) + 1;
        fits.then_some(SubRange { start, count })
    })
}

/// Every account's id in `accounts` text: the third field of
/// `/etc/passwd` (uids) or `/etc/group` (gids). Lines without one are
/// skipped.
#[must_use]
pub fn account_ids(accounts: &str) -> Vec<u32> {
    accounts
        .lines()
        .filter_map(|line| line.split(':').nth(2)?.parse().ok())
        .collect()
}

/// The name of the account with id `id` in `accounts` text, if any.
#[must_use]
pub fn account_name(accounts: &str, id: u32) -> Option<&str> {
    accounts.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        (fields.nth(1)?.parse::<u32>().ok()? == id).then_some(name)
    })
}

/// The id a cell maps its root to: `range`'s first, once the range is
/// known to hold no id it must not.
///
/// # Errors
///
/// [`Error::InvalidIdMap`] if `range` holds 0, `owner`, or any of
/// `accounts`.
pub fn cell_id(range: SubRange, owner: u32, accounts: &[u32]) -> Result<u32, Error> {
    let refused = |what: String| Error::InvalidIdMap {
        reason: format!(
            "the subordinate range {}..{} holds {what}: a cell mapped into it could act as that account; \
             give the owner a range of unused ids in /etc/subuid and /etc/subgid",
            range.start,
            u64::from(range.start) + u64::from(range.count)
        ),
    };
    if range.holds(0) {
        return Err(refused(String::from("id 0")));
    }
    if range.holds(owner) {
        return Err(refused(format!("the owner's id {owner}")));
    }
    if let Some(account) = accounts.iter().find(|&&id| range.holds(id)) {
        return Err(refused(format!("account id {account}")));
    }
    Ok(range.start)
}

/// Checks that the helper at `path` is a regular file (not a symlink)
/// owned by root, with its setuid bit.
///
/// # Errors
///
/// [`Error::InvalidIdMap`], naming the helper and what's wrong.
pub fn check_helper(path: &Path) -> Result<(), Error> {
    let refused = |what: &str| Error::InvalidIdMap {
        reason: format!(
            "{} {what}: install the `uidmap` package (or your system's newuidmap and newgidmap)",
            path.display()
        ),
    };
    let meta = std::fs::symlink_metadata(path).map_err(|_| refused("is missing"))?;
    if !meta.file_type().is_file() {
        return Err(refused("isn't a regular file"));
    }
    if meta.uid() != 0 {
        return Err(refused("isn't owned by root"));
    }
    if meta.mode() & SETUID == 0 {
        return Err(refused("isn't setuid"));
    }
    Ok(())
}

/// The map helpers, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helpers {
    uid: PathBuf,
    gid: PathBuf,
}

impl Helpers {
    /// `newuidmap` at `uid` and `newgidmap` at `gid`, each checked by
    /// [`check_helper`].
    ///
    /// # Errors
    ///
    /// As [`check_helper`].
    pub fn at(uid: &Path, gid: &Path) -> Result<Self, Error> {
        check_helper(uid)?;
        check_helper(gid)?;
        Ok(Self {
            uid: uid.to_path_buf(),
            gid: gid.to_path_buf(),
        })
    }

    /// The standard helpers, [`NEWUIDMAP`] and [`NEWGIDMAP`].
    ///
    /// # Errors
    ///
    /// As [`check_helper`].
    pub fn standard() -> Result<Self, Error> {
        Self::at(Path::new(NEWUIDMAP), Path::new(NEWGIDMAP))
    }

    /// Writes the maps of `child_pid`, a child that just entered a new
    /// user namespace: its 0 to host `uid` and `gid`, one wide, through
    /// the helpers. `setgroups` is set to `deny` first, as for a map the
    /// broker writes itself, so the cell can't drop supplementary groups.
    ///
    /// `child_pid` must be the caller's own unreaped child, so the pid
    /// can't name another process.
    ///
    /// # Errors
    ///
    /// [`Error::IdMapWrite`] if `setgroups` can't be denied, or a helper
    /// can't run or refuses the map.
    pub fn write_maps(&self, child_pid: i32, uid: u32, gid: u32) -> Result<(), Error> {
        let failed = |source: io::Error| Error::IdMapWrite {
            pid: child_pid,
            source,
        };
        std::fs::write(format!("/proc/{child_pid}/setgroups"), "deny").map_err(failed)?;
        for (helper, id) in [(&self.uid, uid), (&self.gid, gid)] {
            let status = Command::new(helper)
                .args([
                    child_pid.to_string(),
                    String::from("0"),
                    id.to_string(),
                    String::from("1"),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(failed)?;
            if !status.success() {
                return Err(failed(io::Error::other(format!(
                    "{} refused the map ({status})",
                    helper.display()
                ))));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_found_by_name_or_id_and_must_be_sound() {
        let subid = "# comment\n\nalice:100000:65536\n1000:300000:10\nbob:x:5\ncarol:5:0\nend:4294967295:2\nwhole:4294967295:1\n";
        assert_eq!(
            range_for(subid, "alice", 1001),
            Some(SubRange {
                start: 100_000,
                count: 65_536
            })
        );
        assert_eq!(
            range_for(subid, "nobody-by-name", 1000),
            Some(SubRange {
                start: 300_000,
                count: 10
            }),
            "by numeric id"
        );
        assert_eq!(range_for(subid, "bob", 2), None, "malformed");
        assert_eq!(range_for(subid, "carol", 3), None, "empty");
        assert_eq!(range_for(subid, "end", 4), None, "runs past u32::MAX");
        assert_eq!(
            range_for(subid, "whole", 5),
            Some(SubRange {
                start: u32::MAX,
                count: 1
            }),
            "ends exactly at u32::MAX"
        );
        assert_eq!(
            range_for("alice:1:2:3\n", "alice", 1),
            None,
            "a fourth field"
        );
        assert_eq!(range_for(subid, "dave", 6), None);
    }

    #[test]
    fn a_range_holds_exactly_its_ids() {
        let range = SubRange {
            start: 10,
            count: 3,
        };
        assert!(!range.holds(9));
        assert!(range.holds(10) && range.holds(12));
        assert!(!range.holds(13));
        let top = SubRange {
            start: u32::MAX,
            count: 1,
        };
        assert!(top.holds(u32::MAX) && !top.holds(u32::MAX - 1));
    }

    #[test]
    fn accounts_are_read_by_their_third_field() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nocean:x:1000:1000::/home/ocean:/bin/bash\nbroken\nodd:x:notanumber:1::/:/bin/sh\n";
        assert_eq!(account_ids(passwd), [0, 1000]);
        assert_eq!(account_name(passwd, 1000), Some("ocean"));
        assert_eq!(account_name(passwd, 0), Some("root"));
        assert_eq!(account_name(passwd, 7), None);
        assert_eq!(account_name("short:x\n", 7), None);
    }

    #[test]
    fn a_cell_maps_to_the_first_id_of_a_range_that_holds_nobody() {
        let range = SubRange {
            start: 200_000,
            count: 65_536,
        };
        assert!(matches!(
            cell_id(range, 1000, &[0, 1000, 65_534]),
            Ok(200_000)
        ));
        let refused = |range, owner, accounts: &[u32]| {
            matches!(
                cell_id(range, owner, accounts),
                Err(Error::InvalidIdMap { .. })
            )
        };
        assert!(
            refused(
                SubRange {
                    start: 0,
                    count: 10
                },
                1000,
                &[]
            ),
            "holds 0"
        );
        assert!(
            refused(
                SubRange {
                    start: 900,
                    count: 200
                },
                1000,
                &[]
            ),
            "holds the owner"
        );
        assert!(
            refused(
                SubRange {
                    start: 60_000,
                    count: 10_000
                },
                1000,
                &[65_534]
            ),
            "holds an account"
        );
        assert!(
            matches!(
                cell_id(
                    SubRange {
                        start: 1001,
                        count: 5
                    },
                    1000,
                    &[1006]
                ),
                Ok(1001)
            ),
            "neighbours on both sides are fine"
        );
    }

    #[test]
    fn a_helper_must_be_a_setuid_regular_file_of_roots() {
        let dir = std::env::temp_dir().join(format!("hatchling-subid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let reason = |path: &Path| match check_helper(path) {
            Err(Error::InvalidIdMap { reason }) => reason,
            other => format!("{other:?}"),
        };
        assert!(reason(&dir.join("missing")).ends_with(
            "is missing: install the `uidmap` package (or your system's newuidmap and newgidmap)"
        ));
        assert!(reason(&dir).contains("isn't a regular file"));
        let mine = dir.join("mine");
        std::fs::write(&mine, b"").unwrap();
        // This test's own file: owned by root only when the test runs as
        // root, and never setuid.
        let euid = crate::process::euid();
        assert!(reason(&mine).contains(if euid == 0 {
            "isn't setuid"
        } else {
            "isn't owned by root"
        }));
        // A regular file of root's that isn't setuid (on every Linux):
        // only the setuid check refuses it.
        let passwd = Path::new("/etc/passwd");
        assert!(
            reason(passwd).contains("isn't setuid"),
            "{}",
            reason(passwd)
        );
        std::os::unix::fs::symlink("/bin/sh", dir.join("link")).unwrap();
        // Both helpers are checked: one bad one refuses the pair.
        assert!(Helpers::at(&mine, &mine).is_err());
        let real = (Path::new(NEWUIDMAP), Path::new(NEWGIDMAP));
        if check_helper(real.0).is_ok() && check_helper(real.1).is_ok() {
            assert_eq!(
                Helpers::at(real.0, real.1).unwrap(),
                Helpers::standard().unwrap()
            );
            assert!(
                Helpers::at(real.0, &mine).is_err(),
                "the gid helper is checked too"
            );
            assert!(Helpers::at(&mine, real.1).is_err(), "and the uid helper");
        }
        assert!(reason(&dir.join("link")).contains("isn't a regular file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether the real helpers are usable here: no `no_new_privs`, user
    /// namespaces allowed, the helpers present and checked, and a
    /// subordinate range for this process. `HATCHLING_REQUIRE_SUBID=1`
    /// turns "no" into a failure instead of a skip.
    fn usable() -> Option<(Helpers, u32, u32)> {
        let required = std::env::var("HATCHLING_REQUIRE_SUBID").is_ok_and(|value| value == "1");
        let found = (|| {
            // A setuid helper gains nothing under no_new_privs, and a
            // kernel or AppArmor that refuses user namespaces leaves no
            // child to map: either way the helpers can't run here.
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            if !status
                .lines()
                .any(|line| line.split_whitespace().eq(["NoNewPrivs:", "0"]))
            {
                return None;
            }
            let unshared = Command::new("unshare").args(["-U", "true"]).status().ok()?;
            if !unshared.success() {
                return None;
            }
            let helpers = Helpers::standard().ok()?;
            let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
            let group_file = std::fs::read_to_string("/etc/group").ok()?;
            let owner = crate::process::euid();
            let primary_gid = rustix::process::getegid().as_raw();
            let name = account_name(&passwd, owner)?;
            let uids = range_for(&std::fs::read_to_string("/etc/subuid").ok()?, name, owner)?;
            let gids = range_for(&std::fs::read_to_string("/etc/subgid").ok()?, name, owner)?;
            let uid = cell_id(uids, owner, &account_ids(&passwd)).ok()?;
            let gid = cell_id(gids, primary_gid, &account_ids(&group_file)).ok()?;
            Some((helpers, uid, gid))
        })();
        assert!(
            found.is_some() || !required,
            "HATCHLING_REQUIRE_SUBID=1, but no usable subordinate range and helpers"
        );
        found
    }

    /// A child in a user namespace of its own, once it's entered it.
    fn in_own_user_namespace() -> (std::process::Child, i32) {
        let child = Command::new("unshare")
            .args(["-U", "sleep", "10"])
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        // Until unshare has made the namespace, it's still in ours.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let own_ns = std::fs::read_link("/proc/self/ns/user").unwrap();
        while std::fs::read_link(format!("/proc/{pid}/ns/user")).is_ok_and(|ns| ns == own_ns)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (child, pid)
    }

    #[test]
    fn the_helpers_map_a_child_into_the_range_with_setgroups_denied() {
        let Some((helpers, uid, gid)) = usable() else {
            return;
        };
        let (mut child, pid) = in_own_user_namespace();
        let mapped = helpers.write_maps(pid, uid, gid);
        let read = |file: &str| std::fs::read_to_string(format!("/proc/{pid}/{file}")).unwrap();
        let (uid_map, gid_map, setgroups) = (read("uid_map"), read("gid_map"), read("setgroups"));
        let _ = child.kill();
        let _ = child.wait();
        mapped.unwrap();
        let fields = |map: &str| map.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(fields(&uid_map), ["0", &uid.to_string(), "1"]);
        assert_eq!(fields(&gid_map), ["0", &gid.to_string(), "1"]);
        assert_eq!(setgroups.trim(), "deny");
        // The owner's own uid isn't in its range: the helper refuses it.
        let (mut other, other_pid) = in_own_user_namespace();
        let refused = helpers.write_maps(other_pid, 0, 0);
        let _ = other.kill();
        let _ = other.wait();
        assert!(
            matches!(refused, Err(Error::IdMapWrite { .. })),
            "{refused:?}"
        );
    }
}
