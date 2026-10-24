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

/// The first range `subid` text grants the account with id `id` (and
/// `name`, if it has one).
///
/// `subid` is `/etc/subuid` or `/etc/subgid`: lines `name:start:count` or
/// `id:start:count`. Comments, blank lines, malformed lines, an empty
/// range and one that runs past `u32::MAX` are skipped.
#[must_use]
pub fn range_for(subid: &str, name: Option<&str>, id: u32) -> Option<SubRange> {
    subid.lines().find_map(|line| {
        let mut fields = line.trim().split(':');
        let (Some(who), Some(start), Some(count), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if Some(who) != name && who.parse::<u32>().ok() != Some(id) {
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

/// The uid and primary gid of the account `name` in `passwd` text
/// (`/etc/passwd`: `name:password:uid:gid:...`), if it has a line there.
#[must_use]
pub fn account(passwd: &str, name: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        let uid = fields.nth(1)?.parse().ok()?;
        let gid = fields.next()?.parse().ok()?;
        Some((uid, gid))
    })
}

/// [`account`] in this host's `/etc/passwd`: a system user is a local one.
///
/// # Errors
///
/// [`Error::InvalidIdMap`] if `/etc/passwd` can't be read.
pub fn local_account(name: &str) -> Result<Option<(u32, u32)>, Error> {
    let passwd = std::fs::read_to_string("/etc/passwd").map_err(|source| Error::InvalidIdMap {
        reason: format!("cannot read /etc/passwd: {source}"),
    })?;
    Ok(account(&passwd, name))
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

/// The account files a cell's subordinate ids are read from, as text.
#[derive(Debug, Clone, Copy)]
pub struct AccountFiles<'a> {
    /// `/etc/passwd`.
    pub passwd: &'a str,
    /// `/etc/group`.
    pub group: &'a str,
    /// `/etc/subuid`.
    pub subuid: &'a str,
    /// `/etc/subgid`.
    pub subgid: &'a str,
}

/// The uid and gid a cell of `owner` maps its root to.
///
/// They're the first ids of the owner's subordinate uid and gid ranges,
/// each checked by [`cell_id`] (the gid against `/etc/group` and
/// `primary_gid`, the owner's primary group). Both ranges are the
/// owner's: `/etc/subgid` names users, as `newgidmap` reads it.
///
/// # Errors
///
/// [`Error::InvalidIdMap`] if the owner has no range in either file
/// (saying how to grant one), or a range is unsafe.
pub fn cell_ids(
    owner: u32,
    primary_gid: u32,
    files: &AccountFiles<'_>,
) -> Result<(u32, u32), Error> {
    let name = account_name(files.passwd, owner);
    let missing = |file: &str| Error::InvalidIdMap {
        reason: format!(
            "uid {owner} has no subordinate range in {file}: grant it one of unused ids \
             (`sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 {}`) \
             so its cells get a uid of their own without root",
            name.unwrap_or("<user>")
        ),
    };
    let uids = range_for(files.subuid, name, owner).ok_or_else(|| missing("/etc/subuid"))?;
    let gids = range_for(files.subgid, name, owner).ok_or_else(|| missing("/etc/subgid"))?;
    Ok((
        cell_id(uids, owner, &account_ids(files.passwd))?,
        cell_id(gids, primary_gid, &account_ids(files.group))?,
    ))
}

/// [`cell_ids`] for this process: its effective uid and gid, and the
/// system's account files.
///
/// # Errors
///
/// As [`cell_ids_for`].
pub fn own_cell_ids() -> Result<(u32, u32), Error> {
    cell_ids_for(crate::process::euid(), rustix::process::getegid().as_raw())
}

/// [`cell_ids`] for `owner`, whose primary group is `primary_gid`, in the
/// system's account files. A missing `/etc/subuid` or `/etc/subgid` holds
/// no range.
///
/// # Errors
///
/// As [`cell_ids`], or [`Error::InvalidIdMap`] if `/etc/passwd` or
/// `/etc/group` can't be read.
pub fn cell_ids_for(owner: u32, primary_gid: u32) -> Result<(u32, u32), Error> {
    let read = |path: &str| {
        std::fs::read_to_string(path).map_err(|source| Error::InvalidIdMap {
            reason: format!("cannot read {path}: {source}"),
        })
    };
    let optional = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let (passwd, group) = (read("/etc/passwd")?, read("/etc/group")?);
    let (uid_ranges, gid_ranges) = (optional("/etc/subuid"), optional("/etc/subgid"));
    cell_ids(
        owner,
        primary_gid,
        &AccountFiles {
            passwd: &passwd,
            group: &group,
            subuid: &uid_ranges,
            subgid: &gid_ranges,
        },
    )
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
    /// the helpers.
    ///
    /// `setgroups` is left as it is (`allow`), unlike a map the broker
    /// writes itself. A cell starts with the owner's supplementary groups
    /// (a `docker` group, say, would open the Docker socket to it), and
    /// it must drop them; with `deny` it couldn't. `deny` exists so an
    /// unprivileged user can't shed a group that a file denies access by;
    /// a map the administrator granted through the helpers isn't that
    /// case, and the kernel allows `setgroups` under one.
    ///
    /// `child_pid` must be the caller's own unreaped child, so the pid
    /// can't name another process.
    ///
    /// # Errors
    ///
    /// [`Error::IdMapWrite`] if a helper can't run or refuses the map.
    pub fn write_maps(&self, child_pid: i32, uid: u32, gid: u32) -> Result<(), Error> {
        self.run(child_pid, &[(0, uid)], &[(0, gid)])
    }

    /// Writes the maps of `child_pid` for a trusted helper that changes
    /// files between the owner and a cell (WO-014, part 2b): its 0 to the
    /// owner's `owner` uid and gid, its 1 to the cell's `cell` uid and gid,
    /// each one wide, in one call per map (a map can't be extended once
    /// written). The owner's own ids need no range: the helpers let a user
    /// map its own uid and primary gid beside its subordinate ones.
    /// `setgroups` stays `allow`, as for [`write_maps`](Self::write_maps).
    ///
    /// `child_pid` must be the caller's own unreaped child.
    ///
    /// # Errors
    ///
    /// [`Error::IdMapWrite`] if a helper can't run or refuses the map.
    pub fn write_helper_maps(
        &self,
        child_pid: i32,
        owner: (u32, u32),
        cell: (u32, u32),
    ) -> Result<(), Error> {
        self.run(
            child_pid,
            &[(0, owner.0), (1, cell.0)],
            &[(0, owner.1), (1, cell.1)],
        )
    }

    /// Runs each helper for `child_pid`: `uids` and `gids` map an id in
    /// the namespace to a host id, one wide each.
    fn run(&self, child_pid: i32, uids: &[(u32, u32)], gids: &[(u32, u32)]) -> Result<(), Error> {
        let failed = |source: io::Error| Error::IdMapWrite {
            pid: child_pid,
            source,
        };
        for (helper, ids) in [(&self.uid, uids), (&self.gid, gids)] {
            let ranges = ids.iter().flat_map(|&(inside, outside)| {
                [inside.to_string(), outside.to_string(), String::from("1")]
            });
            let status = Command::new(helper)
                .arg(child_pid.to_string())
                .args(ranges)
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

    /// This process's cell ids are those of its own uid and gid: the same
    /// answer, or the same refusal, by either way of asking.
    #[test]
    fn own_cell_ids_are_the_cell_ids_for_this_process() {
        let own = own_cell_ids().map_err(|error| error.to_string());
        let named = cell_ids_for(crate::process::euid(), rustix::process::getegid().as_raw())
            .map_err(|error| error.to_string());
        assert_eq!(own, named);
    }

    /// An account is found by its name alone, with its uid and primary
    /// gid; a name that is only a prefix, or a line without both ids,
    /// isn't one.
    #[test]
    fn an_account_is_found_by_its_name() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      hatchling-keeper:x:998:997::/nonexistent:/usr/sbin/nologin\n\
                      hatchling-keeper-old:x:996:995::/:/bin/false\n\
                      broken:x:abc:1::/:/bin/false\n";
        assert_eq!(account(passwd, "hatchling-keeper"), Some((998, 997)));
        assert_eq!(account(passwd, "root"), Some((0, 0)));
        assert_eq!(account(passwd, "hatchling"), None);
        assert_eq!(account(passwd, "broken"), None);
        assert_eq!(account(passwd, "absent"), None);
        assert_eq!(local_account("root").unwrap(), Some((0, 0)));
        assert_eq!(local_account("no-such-hatchling-user").unwrap(), None);
    }

    #[test]
    fn a_range_is_found_by_name_or_id_and_must_be_sound() {
        let subid = "# comment\n\nalice:100000:65536\n1000:300000:10\nbob:x:5\ncarol:5:0\nend:4294967295:2\nwhole:4294967295:1\n";
        assert_eq!(
            range_for(subid, Some("alice"), 1001),
            Some(SubRange {
                start: 100_000,
                count: 65_536
            })
        );
        assert_eq!(
            range_for(subid, Some("nobody-by-name"), 1000),
            Some(SubRange {
                start: 300_000,
                count: 10
            }),
            "by numeric id"
        );
        assert_eq!(range_for(subid, Some("bob"), 2), None, "malformed");
        assert_eq!(range_for(subid, Some("carol"), 3), None, "empty");
        assert_eq!(range_for(subid, Some("end"), 4), None, "runs past u32::MAX");
        assert_eq!(
            range_for(subid, Some("whole"), 5),
            Some(SubRange {
                start: u32::MAX,
                count: 1
            }),
            "ends exactly at u32::MAX"
        );
        assert_eq!(
            range_for("alice:1:2:3\n", Some("alice"), 1),
            None,
            "a fourth field"
        );
        assert_eq!(range_for(subid, Some("dave"), 6), None);
        // An account with no name is found by its id alone: a line with
        // an empty name field is never its.
        assert_eq!(range_for(":7:2\n", None, 6), None);
        assert_eq!(
            range_for(subid, None, 1000),
            Some(SubRange {
                start: 300_000,
                count: 10
            })
        );
    }

    #[test]
    fn a_cells_ids_are_the_first_of_the_owners_ranges() {
        let files = AccountFiles {
            passwd: "root:x:0:0::/root:/bin/sh\ndana:x:1000:1000::/home/dana:/bin/sh\n",
            group: "root:x:0:\ndana:x:1000:\ndocker:x:999:dana\n",
            subuid: "dana:100000:65536\n",
            subgid: "dana:200000:65536\n",
        };
        assert_eq!(cell_ids(1000, 1000, &files).unwrap(), (100_000, 200_000));
        // The gid range is the owner's, found by uid as well as by name.
        let by_id = AccountFiles {
            subuid: "1000:100000:65536\n",
            subgid: "1000:200000:65536\n",
            ..files
        };
        assert_eq!(cell_ids(1000, 50, &by_id).unwrap(), (100_000, 200_000));
        // No range in either file: refused, saying how to grant one.
        let reason = refusal(cell_ids(
            1000,
            1000,
            &AccountFiles {
                subuid: "",
                ..files
            },
        ));
        assert!(
            reason.contains("/etc/subuid") && reason.contains("usermod"),
            "{reason}"
        );
        assert!(reason.contains(" dana`"), "names the user: {reason}");
        let reason = refusal(cell_ids(
            1000,
            1000,
            &AccountFiles {
                subgid: "",
                ..files
            },
        ));
        assert!(reason.contains("/etc/subgid"), "{reason}");
        // Each range is checked against its own accounts.
        let onto_docker = refusal(cell_ids(
            1000,
            1000,
            &AccountFiles {
                subgid: "dana:999:1\n",
                ..files
            },
        ));
        assert!(onto_docker.contains("account id 999"), "{onto_docker}");
        let onto_owner = refusal(cell_ids(
            1000,
            1000,
            &AccountFiles {
                subuid: "dana:1000:5\n",
                ..files
            },
        ));
        assert!(onto_owner.contains("owner"), "{onto_owner}");
        let unnamed = refusal(cell_ids(1234, 1234, &files));
        assert!(unnamed.contains("<user>"), "{unnamed}");
    }

    /// The reason of an [`Error::InvalidIdMap`]; any other result fails.
    fn refusal(result: Result<(u32, u32), Error>) -> String {
        match result {
            Err(Error::InvalidIdMap { reason }) => reason,
            Ok(_) => unreachable!("expected a refusal, got the ids"),
            Err(_) => unreachable!("expected a refusal, got another error"),
        }
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
        let accounts = "root:x:0:0:root:/root:/bin/bash\ndana:x:1000:1000::/home/dana:/bin/bash\nbroken\nodd:x:notanumber:1::/:/bin/sh\n";
        assert_eq!(account_ids(accounts), [0, 1000]);
        assert_eq!(account_name(accounts, 1000), Some("dana"));
        assert_eq!(account_name(accounts, 0), Some("root"));
        assert_eq!(account_name(accounts, 7), None);
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
        let roots_file = Path::new("/etc/passwd");
        assert!(
            reason(roots_file).contains("isn't setuid"),
            "{}",
            reason(roots_file)
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
            let (uid, gid) = own_cell_ids().ok()?;
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
    fn a_child_mapped_through_the_helpers_can_drop_the_owners_groups() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixStream;

        use crate::process::{ForkResult, child_exit_code, exit_child, fork};

        let Some((helpers, uid, gid)) = usable() else {
            return;
        };
        let (parent_end, child_end) = UnixStream::pair().unwrap();
        let child = match fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Its own user namespace, then wait for the maps, then
                // drop every supplementary group: 0 if it could.
                let mut byte = [0_u8; 1];
                let dropped = crate::namespaces::unshare_user().is_ok()
                    && (&child_end).write_all(b"u").is_ok()
                    && (&child_end).read_exact(&mut byte).is_ok()
                    && crate::process::clear_groups().is_ok();
                exit_child(i32::from(!dropped));
            }
        };
        let mut byte = [0_u8; 1];
        (&parent_end).read_exact(&mut byte).unwrap();
        let mapped = helpers.write_maps(child.pid().get(), uid, gid);
        (&parent_end).write_all(b"m").unwrap();
        let status = child.wait_deadline(std::time::Duration::from_secs(10));
        mapped.unwrap();
        assert_eq!(child_exit_code(status.unwrap()), 0, "it dropped its groups");
    }

    /// The tree helper's namespace (WO-014, part 2b): the owner as 0, the
    /// cell as 1, in one map each; in it, a tree goes to the cell's ids
    /// and comes back, a directory the cell left unsearchable included.
    #[test]
    fn a_helper_mapping_owner_and_cell_hands_a_tree_over_and_back() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        use std::os::unix::net::UnixStream;

        use crate::process::{ForkResult, child_exit_code, exit_child, fork};

        let Some((helpers, uid, gid)) = usable() else {
            return;
        };
        let owner = (crate::process::euid(), crate::process::egid());
        let tree =
            std::env::temp_dir().join(format!("hatchling-subid-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        std::fs::create_dir_all(tree.join("inner")).unwrap();
        std::fs::write(tree.join("inner").join("file"), b"x").unwrap();
        // Its root searchable by anyone, so the owner can look inside
        // once it's the cell's; the walk adds its owner's bits to that.
        std::fs::set_permissions(tree.join("inner"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A symlink to a file outside: the walk changes the link, never
        // what it points at.
        let outside =
            std::env::temp_dir().join(format!("hatchling-subid-outside-{}", std::process::id()));
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("link")).unwrap();
        // In a child of its own: a namespace, its maps, then `step` as
        // that namespace's root. Whether it worked, and the maps.
        let in_helper = |step: fn(&Path) -> bool| {
            let (parent_end, child_end) = UnixStream::pair().unwrap();
            let child = match fork().unwrap() {
                ForkResult::Parent { child } => child,
                ForkResult::Child => {
                    let mut byte = [0_u8; 1];
                    let done = crate::namespaces::unshare_user().is_ok()
                        && (&child_end).write_all(b"u").is_ok()
                        && (&child_end).read_exact(&mut byte).is_ok()
                        && crate::become_cell_user().is_ok()
                        && step(&tree);
                    exit_child(i32::from(!done));
                }
            };
            let mut byte = [0_u8; 1];
            (&parent_end).read_exact(&mut byte).unwrap();
            let pid = child.pid().get();
            let mapped = helpers.write_helper_maps(pid, owner, (uid, gid));
            let maps = ["uid_map", "gid_map"].map(|file| {
                std::fs::read_to_string(format!("/proc/{pid}/{file}"))
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            });
            (&parent_end).write_all(b"m").unwrap();
            let status = child.wait_deadline(std::time::Duration::from_secs(10));
            mapped.unwrap();
            (child_exit_code(status.unwrap()) == 0, maps)
        };
        let (given, maps) = in_helper(|tree| {
            crate::chown_tree(tree, 1, 1).is_ok()
                // As the cell might leave it.
                && std::fs::set_permissions(tree.join("inner"), std::fs::Permissions::from_mode(0o0))
                    .is_ok()
        });
        let host = |path: &Path| {
            let meta = std::fs::symlink_metadata(path).unwrap();
            (meta.uid(), meta.gid(), meta.mode() & 0o777)
        };
        let after_give = (host(&tree), host(&tree.join("inner")));
        let outside_after_give = host(&outside);
        let link_after_give = host(&tree.join("link"));
        let (reclaimed, _) = in_helper(|tree| crate::chown_tree(tree, 0, 0).is_ok());
        let after_reclaim = (
            host(&tree),
            host(&tree.join("inner")),
            host(&tree.join("inner").join("file")),
        );
        let removed = std::fs::remove_dir_all(&tree);
        let _ = std::fs::remove_file(&outside);
        let (u, g, s, t) = (owner.0, owner.1, uid, gid);
        assert_eq!(
            maps,
            [
                ["0", &u.to_string(), "1", "1", &s.to_string(), "1"]
                    .map(String::from)
                    .to_vec(),
                ["0", &g.to_string(), "1", "1", &t.to_string(), "1"]
                    .map(String::from)
                    .to_vec(),
            ]
        );
        assert!(given, "the tree went to the cell's ids");
        assert_eq!(after_give, ((s, t, 0o755), (s, t, 0)));
        assert_eq!(
            (link_after_give.0, link_after_give.1),
            (s, t),
            "the link itself"
        );
        assert_eq!(
            (outside_after_give.0, outside_after_give.1),
            (u, g),
            "never what it points at"
        );
        assert!(reclaimed, "and came back");
        assert_eq!(after_reclaim.0, (u, g, 0o755));
        assert_eq!(after_reclaim.1, (u, g, 0o700), "searchable again");
        assert_eq!((after_reclaim.2.0, after_reclaim.2.1), (u, g));
        removed.expect("the owner can remove it all once it's back");
    }

    #[test]
    fn the_helpers_map_a_child_into_the_range_leaving_setgroups_allowed() {
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
        assert_eq!(setgroups.trim(), "allow", "so the cell can drop its groups");
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
