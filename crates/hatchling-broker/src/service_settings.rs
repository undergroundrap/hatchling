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
//! The guided setup (`vault-keyring setup`) is the service's own: it
//! changes the service's settings and restarts it. So it always runs with
//! the service's settings and none of the shell's, whatever the shell
//! holds. Run with a shell's instead, it would stop the service, find no
//! keyring among the settings it was given, and start the service again
//! on settings it had just changed.
//!
//! The file says what a root broker runs and as whom, so it's read only
//! when it's root's and nobody else can write it. One another user could
//! change would let them choose the program a root process starts.

use std::collections::BTreeMap;
use std::ffi::OsString;
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
    read_trusted_text(path, owner).map(|text| parse_settings(&text))
}

/// The text of the settings file at `path`, read only when `owner` owns
/// it and nobody else may write it, as [`read_trusted`] reads it.
pub(crate) fn read_trusted_text(path: &Path, owner: u32) -> Result<String, Unread> {
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
    Ok(text)
}

/// Whether the command `args` runs with the service's settings: an
/// operator command (not serving, and not `doctor`, which reads them
/// itself), from an environment with no settings, where the service has
/// a settings file.
fn wanted(args: &[String], environment_set: bool, service_exists: bool) -> bool {
    let operator = args.first().is_some_and(|command| command != "doctor");
    operator && !environment_set && service_exists
}

/// Whether the environment holds any of the broker's settings.
pub(crate) fn environment_set() -> bool {
    std::env::vars_os().any(|(key, _)| {
        key.to_str()
            .is_some_and(|key| key.starts_with("HATCHLING_"))
    })
}

/// What every one of the broker's settings begins with.
const OURS: &str = "HATCHLING_";

/// The broker's settings among `variables`, an environment's.
fn own_settings(variables: impl Iterator<Item = (OsString, OsString)>) -> BTreeMap<String, String> {
    variables
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            key.starts_with(OURS)
                .then(|| (key, value.to_string_lossy().into_owned()))
        })
        .collect()
}

/// How a command goes on, given the settings it was run with.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// As it is.
    Stay,
    /// Again, with `settings` in its environment and `drop` out of it.
    Again {
        /// The service's settings, all of them.
        settings: BTreeMap<String, String>,
        /// The shell's own, which the service doesn't have.
        drop: Vec<String>,
    },
    /// It can't: why, and the fix.
    Unread(Unread),
}

/// Whether the command `args`, run with the settings `own` in its
/// environment, runs again with the service's, which `read` reads.
///
/// An operator command from a shell with none does ([`wanted`]). The
/// guided setup always does, whatever the shell holds: it's the service's
/// own setup, and changes and restarts it, so a setting left in a shell
/// never stands in for the service's. It runs with exactly the service's,
/// and what a first setup adds to them
/// ([`crate::key_setup::first_settings`]): once it has those, it stays.
fn plan(
    args: &[String],
    own: &BTreeMap<String, String>,
    service_exists: bool,
    read: impl FnOnce() -> Result<BTreeMap<String, String>, Unread>,
) -> Plan {
    let setup = service_exists && crate::key_setup::for_service(args, service_exists);
    if !setup && !wanted(args, !own.is_empty(), service_exists) {
        return Plan::Stay;
    }
    let mut settings = match read() {
        Ok(settings) => settings,
        Err(unread) => return Plan::Unread(unread),
    };
    if setup {
        match crate::key_setup::first_settings(&settings) {
            Ok(added) => settings.extend(added),
            Err(what) => {
                return Plan::Unread(Unread {
                    what,
                    fix: String::from(
                        "Run the installer again: it writes the service's settings, and keeps what's there.",
                    ),
                });
            }
        }
        let theirs = settings.iter().filter(|(key, _)| key.starts_with(OURS));
        if own.iter().eq(theirs) {
            return Plan::Stay;
        }
    }
    let drop = own
        .keys()
        .filter(|key| !settings.contains_key(*key))
        .cloned()
        .collect();
    Plan::Again { settings, drop }
}

/// Runs this command again, with the service's settings as its
/// environment, when [`plan`] says so. `None` when it doesn't; otherwise
/// it returns only when it couldn't: why, and the fix. The service's
/// settings are the file at `service` ([`SERVICE_SETTINGS`]), which
/// `owner` (root) must own.
pub(crate) fn rerun_if_wanted(args: &[String], service: &Path, owner: u32) -> Option<Unread> {
    let own = own_settings(std::env::vars_os());
    let (settings, drop) = match plan(args, &own, service.exists(), || {
        read_trusted(service, owner)
    }) {
        Plan::Stay => return None,
        Plan::Unread(unread) => return Some(unread),
        Plan::Again { settings, drop } => (settings, drop),
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
    let mut again = std::process::Command::new(program);
    again.args(std::env::args_os().skip(1));
    for key in drop {
        again.env_remove(key);
    }
    let err = again.envs(settings).exec();
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
        for command in [
            &["secret", "add", "x"][..],
            &["tools", "list"],
            &["migrate-vault"],
        ] {
            assert!(wanted(&args(command), false, true), "{command:?}");
            // Not with settings of its own, or with no service.
            assert!(!wanted(&args(command), true, true));
            assert!(!wanted(&args(command), false, false));
        }
        // Serving is the service's own; doctor reads the file itself.
        assert!(!wanted(&[], false, true));
        assert!(!wanted(&args(&["doctor"]), false, true));
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// The service's settings on a fresh install: a vault, no keyring, and
    /// a line that isn't the broker's.
    fn fresh() -> BTreeMap<String, String> {
        map(&[
            ("HATCHLING_VAULT_DIR", "/var/lib/hatchling/vault"),
            ("HATCHLING_LOG_LEVEL", "info"),
            ("OTHER", "kept"),
        ])
    }

    /// The broker's own settings among `settings`: what a command run
    /// again finds in its environment.
    fn ours(settings: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        settings
            .iter()
            .filter(|(key, _)| key.starts_with("HATCHLING_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// A read that mustn't happen: one that does shows as a plan that
    /// couldn't read its settings.
    fn unread() -> Result<BTreeMap<String, String>, Unread> {
        Err(Unread {
            what: String::from("the service's settings were read"),
            fix: String::new(),
        })
    }

    /// The settings a command is to run again with, and what it drops;
    /// neither, if it isn't to run again.
    fn again(plan: Plan) -> (BTreeMap<String, String>, Vec<String>) {
        match plan {
            Plan::Again { settings, drop } => (settings, drop),
            Plan::Stay | Plan::Unread(_) => (BTreeMap::new(), vec![String::from("not again")]),
        }
    }

    /// The first run of the guided setup, from a shell that held a setting
    /// of the broker's, changed the service's settings, stopped the
    /// service, and then ran with the shell's settings: no keyring among
    /// them, a usage error, and the service started again on a keyring
    /// that was never made. It runs with the service's, whatever the shell
    /// holds.
    #[test]
    fn the_guided_setup_takes_the_services_settings_whatever_the_shell_holds() {
        for action in ["setup", "lost-key"] {
            let setup = args(&["vault-keyring", action]);
            let shells = [
                (map(&[]), vec![]),
                (
                    map(&[("HATCHLING_LOG_LEVEL", "debug"), ("HATCHLING_WEB", "sites")]),
                    vec![String::from("HATCHLING_WEB")],
                ),
            ];
            for (own, dropped) in shells {
                let (settings, drop) = again(plan(&setup, &own, true, || Ok(fresh())));
                // The service's, all of them, and what a first setup adds.
                let mut wanted = fresh();
                wanted.extend(map(&[
                    (
                        "HATCHLING_VAULT_KEYRING",
                        "/var/lib/hatchling/keyring/keyring",
                    ),
                    ("HATCHLING_REQUIRE_KEY_PIN", "true"),
                ]));
                assert_eq!(settings, wanted, "{action} with {own:?}");
                // The shell's own, which the service doesn't have, go.
                assert_eq!(drop, dropped, "{action} with {own:?}");
            }
        }
    }

    /// The authenticator enrollment, where there's a service, takes the
    /// service's settings as the setup does, whatever the shell holds, and
    /// the keyring a first enrollment needs. Where there's none it stays.
    #[test]
    fn the_authenticator_enrollment_takes_the_services_settings_too() {
        let totp = args(&["vault-keyring", "enroll-totp"]);
        let own = map(&[("HATCHLING_LOG_LEVEL", "debug")]);
        let (settings, drop) = again(plan(&totp, &own, true, || Ok(fresh())));
        assert_eq!(
            settings.get("HATCHLING_VAULT_KEYRING").map(String::as_str),
            Some("/var/lib/hatchling/keyring/keyring")
        );
        assert_eq!(drop, Vec::<String>::new());
        assert_eq!(
            plan(&totp, &ours(&settings), true, || Ok(fresh())),
            Plan::Stay
        );
        assert_eq!(plan(&totp, &own, false, unread), Plan::Stay);
    }

    /// Run again with exactly the service's settings, the setup stays: it
    /// doesn't run a third time. Anything else, and it runs again.
    #[test]
    fn the_guided_setup_runs_again_until_it_has_exactly_the_services_settings() {
        let setup = args(&["vault-keyring", "setup"]);
        let (settings, _) = again(plan(&setup, &map(&[]), true, || Ok(fresh())));
        let own = ours(&settings);
        assert_eq!(own.len(), 4, "{own:?}");
        assert_eq!(plan(&setup, &own, true, || Ok(fresh())), Plan::Stay);

        // One missing, one more and one different: each runs again.
        let mut missing = own.clone();
        missing.remove("HATCHLING_REQUIRE_KEY_PIN");
        let mut more = own.clone();
        more.insert(String::from("HATCHLING_WEB"), String::from("sites"));
        let mut different = own;
        different.insert(String::from("HATCHLING_LOG_LEVEL"), String::from("debug"));
        for shell in [missing, more, different] {
            assert!(
                matches!(
                    plan(&setup, &shell, true, || Ok(fresh())),
                    Plan::Again { .. }
                ),
                "it stayed with {shell:?}"
            );
        }

        // A service with a keyring already: its settings, and nothing added.
        let mut keyed = fresh();
        keyed.insert(
            String::from("HATCHLING_VAULT_KEYRING"),
            String::from("/k/keyring"),
        );
        let (settings, drop) = again(plan(&setup, &map(&[]), true, || Ok(keyed.clone())));
        assert_eq!((settings, drop), (keyed.clone(), vec![]));
        assert_eq!(
            plan(&setup, &ours(&keyed), true, || Ok(keyed.clone())),
            Plan::Stay
        );
    }

    /// Where there's no service, the setup stays and reads nothing (it
    /// says so itself); settings it can't use are said, with the fix.
    #[test]
    fn the_guided_setup_says_what_it_cannot_take() {
        let setup = args(&["vault-keyring", "setup"]);
        assert_eq!(plan(&setup, &map(&[]), false, unread), Plan::Stay);
        let refused = Unread {
            what: String::from("can't read it"),
            fix: String::from("the fix"),
        };
        assert_eq!(
            plan(&setup, &map(&[]), true, || Err(refused.clone())),
            Plan::Unread(refused)
        );
        assert_eq!(
            plan(&setup, &map(&[]), true, || Ok(map(&[("OTHER", "x")]))),
            Plan::Unread(Unread {
                what: String::from("the settings name no vault"),
                fix: String::from(
                    "Run the installer again: it writes the service's settings, and keeps what's there."
                ),
            })
        );
    }

    /// Any other operator command is as it was: from a bare shell it takes
    /// the service's settings, as they are; from a shell with settings of
    /// its own it stays, and reads nothing.
    #[test]
    fn other_commands_take_the_services_settings_only_from_a_bare_shell() {
        for command in [&["tools", "list"][..], &["vault-keyring", "status"]] {
            let command = args(command);
            assert_eq!(
                plan(&command, &map(&[]), true, || Ok(fresh())),
                Plan::Again {
                    settings: fresh(),
                    drop: vec![],
                },
                "{command:?}"
            );
            let own = map(&[("HATCHLING_LOG_LEVEL", "debug")]);
            assert_eq!(
                plan(&command, &own, true, unread),
                Plan::Stay,
                "{command:?}"
            );
            assert_eq!(plan(&command, &map(&[]), false, unread), Plan::Stay);
        }
    }

    /// The settings a command runs again with are the service's file,
    /// trusted as any read of it: one that isn't says so, and nothing runs.
    #[test]
    fn a_command_is_not_run_again_on_settings_it_cannot_trust() {
        let me = hatchling_sys::euid();
        let setup = args(&["vault-keyring", "setup"]);
        // No service: nothing to run again with.
        assert_eq!(
            rerun_if_wanted(&setup, Path::new("/nonexistent/hatchling.env"), me),
            None
        );
        // A file someone else could change: said, and not read.
        let path = settings_file("rerun-open", 0o666);
        let refused = rerun_if_wanted(&setup, &path, me).expect("it went on");
        assert!(refused.what.ends_with("it isn't read"), "{refused:?}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_brokers_own_settings_are_the_ones_named_for_it() {
        let variables = [
            ("HATCHLING_WEB", "sites"),
            ("PATH", "/usr/bin"),
            ("HATCHLING_LOG_LEVEL", ""),
            ("NOT_HATCHLING_WEB", "off"),
            ("SUDO_UID", "1000"),
        ]
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        assert_eq!(
            own_settings(variables.into_iter()),
            map(&[("HATCHLING_LOG_LEVEL", ""), ("HATCHLING_WEB", "sites")])
        );
        assert_eq!(own_settings(std::iter::empty()), map(&[]));
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
        std::fs::write(
            &path,
            "# the service's\nHATCHLING_VAULT_DIR=/var/lib/hatchling/vault\n",
        )
        .unwrap();
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
        assert!(
            missing
                .what
                .starts_with("can't read /nonexistent/hatchling.env"),
            "{missing:?}"
        );
    }
}
