//! A harness's work tree on the owner's subordinate ids (WO-014, part 2b).
//!
//! A harness cell writes in its work tree and scratch directory, so they
//! must be its own: a root broker `chown`s them to the cell's uid
//! ([`crate::harness`]). An unprivileged broker can't give a file away, and
//! after the run it couldn't read, nor remove, what the cell left under its
//! subordinate ids. Rootless containers solve this with a helper in a user
//! namespace that maps both: the owner as its 0 and the cell as its 1. This
//! is that helper.
//!
//! The broker starts itself again as `harness-tree give <dir>...` (before
//! the run) or `harness-tree reclaim <dir>...` (after it, once the cell
//! is gone), a fresh single-threaded process: ordinary
//! Rust after a fork of the multi-threaded broker wouldn't be sound. The
//! helper enters a user namespace of its own and says so; the broker writes
//! its two-identity maps through `newuidmap` and `newgidmap`; the helper
//! becomes its namespace's root and gives each tree to its 1 (`give`) or
//! its 0 (`reclaim`), through [`hatchling_sys::chown_tree`]. The cell never
//! runs in the helper's namespace.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use hatchling_isolation::{CellIdentity, IsolationBackend as _, Supervised as _};

/// The broker's argv\[1\] for the helper.
pub const HARNESS_TREE_SUBCOMMAND: &str = "harness-tree";

/// How long the helper has, from its start, to change its trees.
pub(crate) const HELPER_TIMEOUT: Duration = Duration::from_secs(120);

/// What the helper does with its trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeOp {
    /// Gives them to the cell: its namespace's 1.
    Give,
    /// Takes them back for the broker: its namespace's 0.
    Reclaim,
}

impl TreeOp {
    /// Its name on the helper's command line.
    const fn name(self) -> &'static str {
        match self {
            Self::Give => "give",
            Self::Reclaim => "reclaim",
        }
    }

    /// The operation named `name`, if any.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "give" => Some(Self::Give),
            "reclaim" => Some(Self::Reclaim),
            _ => None,
        }
    }

    /// The uid and gid, in the helper's namespace, the trees go to.
    const fn ids(self) -> (u32, u32) {
        match self {
            Self::Give => (1, 1),
            Self::Reclaim => (0, 0),
        }
    }
}

/// The helper's command line after its subcommand: an operation and at
/// least one absolute directory.
///
/// # Errors
///
/// Why it isn't one.
fn parse_args(args: &[String]) -> Result<(TreeOp, Vec<PathBuf>), String> {
    let (op, dirs) = args
        .split_first()
        .ok_or_else(|| String::from("harness-tree takes give or reclaim, then directories"))?;
    let op = TreeOp::parse(op).ok_or_else(|| format!("harness-tree can't {op:?}"))?;
    if dirs.is_empty() {
        return Err(String::from("harness-tree takes at least one directory"));
    }
    let dirs: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
    if let Some(relative) = dirs.iter().find(|dir| !dir.is_absolute()) {
        return Err(format!("{} isn't absolute", relative.display()));
    }
    Ok((op, dirs))
}

/// The helper: `hatchling-broker harness-tree give|reclaim <dir>...`.
#[must_use]
pub fn harness_tree_main(args: &[String]) -> ExitCode {
    match helper(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            let _ = writeln!(std::io::stderr(), "hatchling-broker harness-tree: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// The helper's work, as the module docs describe.
fn helper(args: &[String]) -> Result<(), String> {
    let (op, dirs) = parse_args(args)?;
    hatchling_sys::unshare_user().map_err(|e| format!("unshare(user): {e}"))?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(b"u")
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("say the namespace is made: {e}"))?;
    let mut go = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut go)
        .map_err(|e| format!("wait for the maps: {e}"))?;
    if go != *b"g" {
        return Err(String::from("the broker didn't write the maps"));
    }
    hatchling_sys::become_cell_user().map_err(|e| format!("become the namespace's root: {e}"))?;
    let (uid, gid) = op.ids();
    for dir in &dirs {
        hatchling_sys::chown_tree(dir, uid, gid)
            .map_err(|e| format!("{} {}: {e}", op.name(), dir.display()))?;
    }
    Ok(())
}

/// Runs the helper at `spawner` (the broker's own binary) to `op` the
/// directories `dirs` between the broker and the cell `cell`, a cell on
/// the owner's subordinate ids.
///
/// # Errors
///
/// Why the trees weren't changed, all of them: the helpers missing, the
/// helper refused a map, or its walk failed (a tree may then be partly
/// changed, and must not be used).
pub(crate) fn change_trees(
    spawner: &Path,
    op: TreeOp,
    dirs: &[&Path],
    cell: CellIdentity,
) -> Result<(), String> {
    let helpers = hatchling_sys::subid::Helpers::standard().map_err(|e| e.to_string())?;
    let mut std_child = Command::new(spawner)
        .arg(HARNESS_TREE_SUBCOMMAND)
        .arg(op.name())
        .args(dirs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", spawner.display()))?;
    let (Some(mut stdin), Some(mut stdout), Some(stderr)) = (
        std_child.stdin.take(),
        std_child.stdout.take(),
        std_child.stderr.take(),
    ) else {
        return Err(String::from("the tree helper's pipes weren't made"));
    };
    // Its pid names it to the map helpers; it is this process's own
    // unreaped child, so the pid can't name another.
    let pid = i32::try_from(std_child.id()).map_err(|_| String::from("the tree helper's pid"))?;
    let child = hatchling_isolation::platform()
        .supervise(&mut std_child)
        .map_err(|e| format!("cannot supervise the tree helper: {e}"))?;
    drop(std_child);
    let mut made = [0_u8; 1];
    let ready = stdout.read_exact(&mut made).is_ok() && made == *b"u";
    let mapped = ready
        && helpers
            .write_helper_maps(
                pid,
                (hatchling_sys::euid(), hatchling_sys::egid()),
                (cell.principal, cell.group),
            )
            .is_ok();
    let _ = stdin.write_all(if mapped { b"g" } else { b"n" });
    drop(stdin);
    let exit = child.wait_deadline(HELPER_TIMEOUT);
    let mut said = String::new();
    let _ = stderr.take(4096).read_to_string(&mut said);
    match exit {
        Ok(exit) if exit.code == 0 && mapped => Ok(()),
        Ok(_) if !mapped => Err(String::from(
            "the tree helper's maps couldn't be written: is the owner's range in /etc/subuid and /etc/subgid?",
        )),
        Ok(exit) => Err(format!(
            "the tree helper exited with status {}: {}",
            exit.code,
            said.trim()
        )),
        Err(e) => Err(format!("the tree helper didn't finish: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|&value| value.to_owned()).collect()
    }

    #[test]
    fn the_helper_takes_an_operation_and_absolute_directories() {
        assert_eq!(
            parse_args(&args(&["give", "/a", "/b"])).unwrap(),
            (TreeOp::Give, vec![PathBuf::from("/a"), PathBuf::from("/b")])
        );
        assert_eq!(
            parse_args(&args(&["reclaim", "/a"])).unwrap().0,
            TreeOp::Reclaim
        );
        for refused in [
            args(&[]),
            args(&["give"]),
            args(&["take", "/a"]),
            args(&["give", "relative"]),
            args(&["reclaim", "/a", "b"]),
        ] {
            assert!(parse_args(&refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_helper_with_bad_arguments_or_no_binary_changes_nothing() {
        assert!(helper(&args(&["take", "/a"])).is_err());
        assert_eq!(
            format!("{:?}", harness_tree_main(&args(&[]))),
            format!("{:?}", ExitCode::FAILURE)
        );
        let identity = CellIdentity::direct(hatchling_sys::euid(), 1);
        let missing = change_trees(
            Path::new("/nonexistent/hatchling-broker"),
            TreeOp::Give,
            &[Path::new("/tmp")],
            identity,
        )
        .unwrap_err();
        assert!(
            missing.contains("cannot start") || missing.contains("newuidmap"),
            "{missing}"
        );
    }

    #[test]
    fn give_is_the_cells_and_reclaim_the_brokers() {
        assert_eq!(TreeOp::Give.ids(), (1, 1));
        assert_eq!(TreeOp::Reclaim.ids(), (0, 0));
        for op in [TreeOp::Give, TreeOp::Reclaim] {
            assert_eq!(TreeOp::parse(op.name()), Some(op));
        }
        assert_eq!(TreeOp::parse("chown"), None);
    }
}
