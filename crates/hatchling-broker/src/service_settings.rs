//! The service's settings, for an operator command run from a shell.
//!
//! Installed as a service, the broker's settings are in
//! [`SERVICE_SETTINGS`], which systemd gives it; a command run from a
//! shell (`sudo hatchling-broker secret add ...`) has none of them. Rather
//! than read some itself, the command runs again with all of them as its
//! environment, so every setting it reads is the service's. A command
//! with only some could take a keyring's absence for a bare key file, and
//! make a new key.
//!
//! The file says what a root broker runs and as whom, so it's read only
//! when it's root's and nobody else can write it. One another user could
//! change would let them choose the program a root process starts.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;

use crate::doctor::{SERVICE_SETTINGS, parse_settings};

/// Why a settings file wasn't read, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unread {
    /// What's wrong.
    pub(crate) what: String,
    /// The fix.
    pub(crate) fix: String,
}

/// The settings in the file at `path`, read only when `owner` owns it and
/// nobody else may write it. Its ownership and mode are the opened
/// file's, so it can't be swapped for another between the check and the
/// read.
pub(crate) fn read_trusted(path: &Path, owner: u32) -> Result<BTreeMap<String, String>, Unread> {
    let shown = path.display();
    let mut file = std::fs::File::open(path).map_err(|err| Unread {
        what: format!("can't read {shown}: {err}"),
        fix: String::from(if err.kind() == std::io::ErrorKind::PermissionDenied {
            "It's root's: run this with sudo."
        } else {
            "Name the broker's settings file with --env, or set them in the environment."
        }),
    })?;
    let unread = |err: std::io::Error| Unread {
        what: format!("can't read {shown}: {err}"),
        fix: String::from("Check the file: it should be a text file of settings."),
    };
    let found = file.metadata().map_err(unread)?;
    if found.uid() != owner || found.mode() & 0o022 != 0 {
        return Err(Unread {
            what: format!(
                "{shown} can be changed by someone else, and says what the broker runs: it isn't read"
            ),
            fix: format!(
                "Make it its owner's alone: `sudo chown {owner}:{owner} {shown}`, then `sudo chmod 600 {shown}`."
            ),
        });
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(unread)?;
    Ok(parse_settings(&text))
}

/// Whether the command `args` runs with the service's settings: an
/// operator command (not serving, and not `doctor`, which reads them
/// itself), from an environment with no settings, where the service has
/// a settings file.
fn wanted(args: &[String], environment_set: bool, service_exists: bool) -> bool {
    let operator = args
        .first()
        .is_some_and(|command| command != "doctor");
    operator && !environment_set && service_exists
}

/// Whether the environment holds any of the broker's settings.
pub(crate) fn environment_set() -> bool {
    std::env::vars_os().any(|(key, _)| {
        key.to_str()
            .is_some_and(|key| key.starts_with("HATCHLING_"))
    })
}

/// Runs this command again, with the service's settings as its
/// environment, when [`wanted`]. `None` when it isn't; otherwise it
/// returns only when it couldn't: why, and the fix.
pub(crate) fn rerun_if_wanted(args: &[String]) -> Option<Unread> {
    let service = Path::new(SERVICE_SETTINGS);
    if !wanted(args, environment_set(), service.exists()) {
        return None;
    }
    let settings = match read_trusted(service, 0) {
        Ok(settings) => settings,
        Err(unread) => return Some(unread),
    };
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(err) => {
            return Some(Unread {
                what: format!("can't find this program to run it again: {err}"),
                fix: format!("Run it with the settings in {SERVICE_SETTINGS} in its environment."),
            });
        }
    };
    let _ = writeln!(
        std::io::stderr().lock(),
        "(with the service's settings, from {SERVICE_SETTINGS})"
    );
    // The arguments as given, the program's own name aside: `hatchling
    // broker ...` keeps its `broker`.
    let err = std::process::Command::new(program)
        .args(std::env::args_os().skip(1))
        .envs(settings)
        .exec();
    Some(Unread {
        what: format!("can't run again with the service's settings: {err}"),
        fix: format!("Run it with the settings in {SERVICE_SETTINGS} in its environment."),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn an_operator_command_from_a_bare_shell_takes_the_services_settings() {
        for command in [&["secret", "add", "x"][..], &["tools", "list"], &["migrate-vault"]] {
            assert!(wanted(&args(command), false, true), "{command:?}");
            // Not with settings of its own, or with no service.
            assert!(!wanted(&args(command), true, true));
            assert!(!wanted(&args(command), false, false));
        }
        // Serving is the service's own; doctor reads the file itself.
        assert!(!wanted(&[], false, true));
        assert!(!wanted(&args(&["doctor"]), false, true));
    }

    /// A settings file in a fresh directory, with `mode`.
    fn settings_file(name: &str, mode: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-service-settings-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hatchling.env");
        std::fs::write(&path, "# the service's\nHATCHLING_VAULT_DIR=/var/lib/hatchling/vault\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn settings_are_read_only_from_a_file_nobody_else_can_change() {
        let me = hatchling_sys::euid();
        for mode in [0o600, 0o644, 0o400] {
            let path = settings_file(&format!("mode-{mode:o}"), mode);
            let read = read_trusted(&path, me).unwrap();
            assert_eq!(
                read.get("HATCHLING_VAULT_DIR").map(String::as_str),
                Some("/var/lib/hatchling/vault")
            );
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
        // Writable by its group or by anyone: not read.
        for mode in [0o620, 0o602, 0o666] {
            let path = settings_file(&format!("open-{mode:o}"), mode);
            let refused = read_trusted(&path, me).unwrap_err();
            assert!(refused.what.ends_with("it isn't read"), "{refused:?}");
            assert!(refused.fix.contains("chmod 600"), "{refused:?}");
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
        // Someone else's: not read either.
        let path = settings_file("theirs", 0o600);
        let refused = read_trusted(&path, me.wrapping_add(1)).unwrap_err();
        assert!(refused.what.ends_with("it isn't read"), "{refused:?}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        // None at all.
        let missing = read_trusted(Path::new("/nonexistent/hatchling.env"), me).unwrap_err();
        assert!(missing.what.starts_with("can't read /nonexistent/hatchling.env"), "{missing:?}");
    }
}
