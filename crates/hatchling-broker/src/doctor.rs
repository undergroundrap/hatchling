//! `doctor`: what stands between this machine and a working Hatchling,
//! each problem with its fix (WO-033).
//!
//! A first start fails on a handful of things: a setting that doesn't
//! parse, the agent's program missing, a kernel or `AppArmor` that won't
//! let the broker make a cell, a model server that isn't running or
//! hasn't the model named. The broker reports each as it meets it, one
//! at a time and in its own words; `doctor` checks them all at once and
//! says what to do about each.
//!
//! It reads the broker's settings from the environment, or, when the
//! environment holds none, from the service's file
//! ([`SERVICE_SETTINGS`]), which is root's: run it with `sudo` there. It
//! changes nothing. It takes no lock (a broker starting meanwhile must
//! not find one held), reads no secret, and writes only its cell probe's
//! staging directory, which it removes. Its only connections are to the
//! model's address, and only to one in plain HTTP: a hosted model's key
//! is the vault's, and `doctor` doesn't ask for it. It asks the server
//! for its list of models, and Ollama for the window it gives the model
//! once that's loaded ([`crate::model_window`]).

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use hatchling_isolation::{CellIdentity, Mapping};

use crate::Config;
use crate::environment as env;

/// How the cell probe's staging directory is named, before its random
/// part.
const STAGE_PREFIX: &str = "hatchling-doctor-cell-";

/// The service's settings, as `scripts/install.sh` writes them.
pub const SERVICE_SETTINGS: &str = "/etc/hatchling/hatchling.env";

/// How long the model's server has to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long it has to answer, once connected.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of its answer read: a list of models is far smaller.
const MAX_ANSWER: u64 = 4 << 20;

/// How many of a server's models a problem names.
const MODELS_SHOWN: usize = 8;

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Finding {
    /// Nothing wrong, and what it saw.
    Fine(String),
    /// Something that stops Hatchling working, and how to fix it.
    Problem {
        /// What is wrong.
        what: String,
        /// What to do about it.
        fix: String,
    },
    /// Not checked, and why.
    Unchecked(String),
}

impl Finding {
    fn problem(what: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Problem {
            what: what.into(),
            fix: fix.into(),
        }
    }
}

/// Where the settings came from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The process's environment.
    Environment,
    /// A settings file, in systemd's `EnvironmentFile` form.
    File(PathBuf),
}

/// Runs `doctor` with `args`, the arguments after its name: none, or
/// `--env <file>`. Exits 0 when it found nothing wrong, 1 otherwise.
#[must_use]
pub fn doctor_main(args: &[String]) -> ExitCode {
    let environment_set = crate::service_settings::environment_set();
    let source = source(args, environment_set, Path::new(SERVICE_SETTINGS));
    if source == Source::Environment && !environment_set {
        let finding = Finding::problem(
            format!("none: the environment has none, and there is no {SERVICE_SETTINGS}"),
            "Install Hatchling as a service (docs/install.md), or set them as the README's quickstart does.",
        );
        return report("the environment", &[("settings", finding)]);
    }
    // The settings as one map, from either: the environment's as it is
    // now (the variables that aren't text can't be settings).
    let (label, settings) = match &source {
        Source::Environment => (
            String::from("the environment"),
            std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .collect::<BTreeMap<String, String>>(),
        ),
        // As the broker would read it: its runner's own, and nobody else's
        // to change.
        Source::File(path) => {
            match crate::service_settings::read_trusted(path, hatchling_sys::euid()) {
                Ok(settings) => (path.display().to_string(), settings),
                Err(unread) => {
                    let finding = Finding::problem(unread.what, unread.fix);
                    return report(&path.display().to_string(), &[("settings", finding)]);
                }
            }
        }
    };
    let lookup = |key: &str| settings.get(key).cloned();
    let checks = [
        ("settings", check_settings(&lookup)),
        (
            "agent program",
            check_program(lookup(env::HATCHLING_AGENT_BIN.name).as_deref()),
        ),
        ("cells", check_cells(&lookup)),
        ("model", {
            let endpoint = lookup(env::HATCHLING_SCHEDULER_MODEL_ENDPOINT.name);
            let name = lookup(env::HATCHLING_SCHEDULER_MODEL_NAME.name);
            let found = check_model(endpoint.as_deref(), name.as_deref());
            with_window(found, endpoint.as_deref(), name.as_deref())
        }),
        ("other model", check_other(&lookup)),
        ("vault", check_vault(&lookup, &Machine::here())),
        ("memory", check_memory(crate::model_window::memory())),
    ];
    report(&label, &checks)
}

/// Where to read the settings: the file `--env` names; else the
/// environment, when it holds any; else the service's file, when there
/// is one.
fn source(args: &[String], environment_set: bool, service: &Path) -> Source {
    match args {
        [flag, file] if flag == "--env" => Source::File(PathBuf::from(file)),
        _ if environment_set || !service.exists() => Source::Environment,
        _ => Source::File(service.to_path_buf()),
    }
}

/// The settings in `text`, in systemd's `EnvironmentFile` form: a
/// `KEY=VALUE` a line, `#` and `;` lines comments, a value's surrounding
/// quotes not part of it.
pub(crate) fn parse_settings(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            let value = value.trim();
            let unquoted = ['"', '\'']
                .into_iter()
                .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
                .unwrap_or(value);
            (key.trim().to_owned(), unquoted.to_owned())
        })
        .collect()
}

/// Whether the settings parse as the broker parses them.
fn check_settings(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    match Config::parse(lookup) {
        Ok(_) => Finding::Fine(String::from("they parse")),
        Err(err) => Finding::problem(
            err.to_string(),
            "Correct it: `hatchling-broker --help` describes every setting.",
        ),
    }
}

/// Whether the other model's server, when the operator set one up, is one
/// the app can switch to: egress grants it to runs, and to the chat.
fn check_other(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    let set = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
    let (Some(endpoint), Some(name)) = (
        set(env::HATCHLING_SCHEDULER_MODEL_OTHER_ENDPOINT.name),
        set(env::HATCHLING_SCHEDULER_MODEL_OTHER_NAME.name),
    ) else {
        return Finding::Fine(String::from(
            "none set up: `use-hosted` or `use-local` keeps the server it switches from, to switch back to in the app",
        ));
    };
    let Some(destination) = crate::model_choice::destination(&endpoint) else {
        return Finding::problem(
            format!("can't read {endpoint} as a model's address"),
            "Set HATCHLING_SCHEDULER_MODEL_OTHER_ENDPOINT to an https:// API's base, or http://localhost:<port>/<path>.",
        );
    };
    let listed = |key: &str, item: &str| {
        set(key).is_some_and(|list| list.split(',').map(str::trim).any(|entry| entry == item))
    };
    let chat = set(env::HATCHLING_API_GRANTS.name).is_some_and(|grants| {
        grants
            .split(',')
            .map(str::trim)
            .any(|grant| grant == "channel-send:local")
    });
    let granted = listed(env::HATCHLING_EGRESS_GRANTS.name, &destination)
        && (!chat
            || listed(
                env::HATCHLING_API_GRANTS.name,
                &format!("egress:{destination}"),
            ));
    if !granted {
        return Finding::problem(
            format!(
                "{name} at {endpoint} is set up, but egress doesn't grant {destination}: the app can't switch to it"
            ),
            format!(
                "Add {destination} to HATCHLING_EGRESS_ALLOWLIST and HATCHLING_EGRESS_GRANTS, and egress:{destination} to HATCHLING_API_GRANTS; or switch to it once with `use-hosted`/`use-local`, which does."
            ),
        );
    }
    Finding::Fine(format!(
        "{name} at {endpoint}: switch to it in the app, under Settings, General, Model"
    ))
}

/// What [`check_vault`] looks at on this machine, apart from the settings.
struct Machine {
    /// The security keys visible here, by hidraw node.
    keys: Vec<String>,
    /// Whether this is WSL, where a key must be attached to be seen.
    wsl: bool,
}

impl Machine {
    fn here() -> Self {
        Self {
            keys: hatchling_fido2::linux::Hidraw::new(Box::new(|| None))
                .nodes()
                .unwrap_or_default(),
            wsl: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft")),
        }
    }
}

/// How the vault opens, and whether that needs the owner after a restart:
/// its key file does it by itself; a code or a security key needs them
/// (`sudo hatchling-broker unlock`). A vault waiting for that now is the
/// problem to fix.
fn check_vault(lookup: &dyn Fn(&str) -> Option<String>, machine: &Machine) -> Finding {
    let Ok(keyring) = crate::vault_keyring::KeyringConfig::parse(lookup) else {
        return Finding::Unchecked(String::from(
            "the keyring's settings don't parse (see settings)",
        ));
    };
    let Some(path) = keyring.keyring.as_deref() else {
        return Finding::Fine(String::from(
            "opens with its key file, by itself after a restart; a security key protects it more (docs/security-keys.md)",
        ));
    };
    // Run as root, the keyring is the keeper's user's: read as theirs,
    // and nothing is made or given to anyone.
    if let (Ok(Some((uid, _))), Some(dir)) = (crate::keeper::keeper_user(), path.parent()) {
        let _ = hatchling_vault::act_for(uid, &[dir]);
    }
    let ring = match hatchling_vault::Keyring::load(path) {
        Ok(ring) => ring,
        Err(error) => {
            return Finding::problem(
                format!("the keyring at {} doesn't load: {error}", path.display()),
                "Check HATCHLING_VAULT_KEYRING names the keyring, and that it's the vault keeper's (docs/security-keys.md).",
            );
        }
    };
    let waiting = Config::parse(lookup)
        .is_ok_and(|config| crate::unlock_relay::socket_path(&config.vault).exists());
    let found = vault_said(ring.tier(), ring.hardware_keys().len(), machine, waiting);
    match found {
        Finding::Fine(said) if crate::key_setup::codes_unconfirmed(path) => Finding::Fine(format!(
            "{said}. Your backup codes were never confirmed saved: vault-keyring setup makes a fresh set"
        )),
        other @ (Finding::Fine(_) | Finding::Problem { .. } | Finding::Unchecked(_)) => other,
    }
}

/// What doctor says of a vault at `tier` with `enrolled` security keys,
/// on `machine`, `waiting` to be unlocked or not.
fn vault_said(
    tier: hatchling_vault::Tier,
    enrolled: usize,
    machine: &Machine,
    waiting: bool,
) -> Finding {
    use hatchling_vault::Tier;
    if waiting {
        let fix = match (tier, machine.keys.is_empty(), machine.wsl) {
            (Tier::HardwareKey, true, true) => {
                "Attach your security key to WSL (in PowerShell: usbipd attach --wsl --busid <id>), then run `sudo hatchling-broker unlock` and touch it. A backup code works too."
            }
            (Tier::HardwareKey, _, _) => {
                "Run `sudo hatchling-broker unlock`, and touch your security key when it flashes. A backup code works too."
            }
            _ => {
                "Run `sudo hatchling-broker unlock`, with a current code from your authenticator app."
            }
        };
        return Finding::problem(
            "locked: it started with nobody to unlock it, and waits for you",
            fix,
        );
    }
    match tier {
        Tier::File => Finding::Fine(String::from(
            "a keyring at the file tier: it opens by itself after a restart",
        )),
        Tier::Totp => Finding::Fine(String::from(
            "opens with a code from your authenticator app: after each restart, `sudo hatchling-broker unlock`",
        )),
        Tier::HardwareKey => {
            let mut said = format!(
                "opens with {enrolled} security key{}: after each restart, `sudo hatchling-broker unlock` and a touch",
                if enrolled == 1 { "" } else { "s" }
            );
            if enrolled == 1 {
                said.push_str(". Only one key: add a second (vault-keyring add-key) and keep your backup codes, so losing it doesn't lock you out");
            }
            if machine.keys.is_empty() {
                said.push_str(if machine.wsl {
                    ". None is visible here now: in WSL, attach it with usbipd to unlock"
                } else {
                    ". None is plugged in now: plug one in to unlock"
                });
            }
            Finding::Fine(said)
        }
    }
}

/// Whether `bin`, the agent's program, is a program.
fn check_program(bin: Option<&str>) -> Finding {
    let Some(bin) = bin else {
        return Finding::problem(
            "HATCHLING_AGENT_BIN isn't set: a cell has no program to run",
            "Set it to the installed hatchling-agent, such as /usr/local/bin/hatchling-agent.",
        );
    };
    match std::fs::metadata(bin) {
        Err(err) => Finding::problem(
            format!("{bin}: {err}"),
            "Install hatchling-agent there, or set HATCHLING_AGENT_BIN to where it is.",
        ),
        Ok(found) if !found.is_file() || found.permissions().mode() & 0o111 == 0 => {
            Finding::problem(
                format!("{bin} isn't a program"),
                "Set HATCHLING_AGENT_BIN to the hatchling-agent program itself.",
            )
        }
        Ok(_) => Finding::Fine(bin.to_owned()),
    }
}

/// Whether this machine lets the broker make a cell, as the settings'
/// cell user.
fn check_cells(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    check_cells_in(lookup, &std::env::temp_dir())
}

/// [`check_cells`], staging the probe in `temp`.
fn check_cells_in(lookup: &dyn Fn(&str) -> Option<String>, temp: &Path) -> Finding {
    let identity = match crate::cell::agent_identity(&lookup) {
        Ok((identity, _)) => identity,
        Err(err) => {
            return Finding::problem(
                format!("the cells' user: {err}"),
                "The README's \"Running without root\" says how cells get a user of their own.",
            );
        }
    };
    // A name nobody can guess, in the shared temp directory: the probe
    // makes it exclusively, so one made there first stops only the probe.
    let stage = match crate::approval::random_hex(STAGE_PREFIX) {
        Ok(name) => temp.join(name),
        Err(err) => {
            return Finding::problem(
                format!("the cell probe can't name its directory: {err}"),
                "Check that /dev/urandom can be read.",
            );
        }
    };
    let probed = probe_cell(&identity, &stage);
    // Made as the cells' user, and empty: what the probe mounted went
    // with its mount namespace.
    let _ = std::fs::remove_dir(&stage);
    match probed {
        Ok(()) if identity.mapping == Mapping::Subordinate => Finding::Fine(String::from(
            "namespaces work (the range's map helpers aren't tried here)",
        )),
        Ok(()) => Finding::Fine(format!("a cell runs as uid {}", identity.principal)),
        Err((step, errno)) => {
            let (what, fix) = cell_advice(step, errno, Facts::read());
            Finding::problem(what, fix)
        }
    }
}

/// A step of making a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Its user namespace.
    User,
    /// Its id maps, which the broker writes from outside.
    Maps,
    /// Its mount namespace.
    Mount,
    /// Its network namespace.
    Net,
    /// Its IPC namespace.
    Ipc,
    /// Its own root filesystem.
    Root,
    /// Its pid namespace.
    Pid,
    /// None: the probe ended before saying which.
    Ended,
}

impl Step {
    const ALL: [Self; 8] = [
        Self::User,
        Self::Maps,
        Self::Mount,
        Self::Net,
        Self::Ipc,
        Self::Root,
        Self::Pid,
        Self::Ended,
    ];

    /// Its code on the probe's socket: 0 is "done".
    const fn code(self) -> u8 {
        match self {
            Self::User => 1,
            Self::Maps => 2,
            Self::Mount => 3,
            Self::Net => 4,
            Self::Ipc => 5,
            Self::Root => 6,
            Self::Pid => 7,
            Self::Ended => 8,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|step| step.code() == code)
            .unwrap_or(Self::Ended)
    }
}

/// A step that failed, and its errno when it had one.
type Failed = (Step, Option<i32>);

/// The errno under a `hatchling-sys` error.
fn errno(err: &hatchling_sys::Error) -> Option<i32> {
    std::error::Error::source(err)?
        .downcast_ref::<std::io::Error>()?
        .raw_os_error()
}

/// The errno as a byte for the probe's socket: every errno is under 256.
fn errno_byte(errno: Option<i32>) -> u8 {
    errno
        .and_then(|errno| u8::try_from(errno).ok())
        .unwrap_or(0)
}

/// Makes the environment-dependent half of a cell at `stage`, as the
/// spawn does, in a child that ends with it: a user namespace, its maps
/// written from outside, then the mount, network, IPC and pid
/// namespaces and the cell's own root. The child reports each step's
/// outcome on a socket; where one fails, it says which, and its errno.
///
/// A cell on a subordinate range is mapped by its helpers, which this
/// doesn't run: it maps the caller's own ids instead, to try the rest.
fn probe_cell(identity: &CellIdentity, stage: &Path) -> Result<(), Failed> {
    let (uid, gid) = match identity.mapping {
        Mapping::Direct => (identity.principal, identity.group),
        Mapping::Subordinate => (hatchling_sys::euid(), hatchling_sys::egid()),
    };
    let maps = hatchling_sys::IdMap::new(uid, 1)
        .and_then(|users| Ok((users, hatchling_sys::IdMap::new(gid, 1)?)))
        .map_err(|err| (Step::Maps, errno(&err)))?;
    let (mut ours, mut theirs) =
        UnixStream::pair().map_err(|err| (Step::Ended, err.raw_os_error()))?;
    match hatchling_sys::fork() {
        Ok(hatchling_sys::ForkResult::Child) => {
            drop(ours);
            let outcome = cell_child(&mut theirs, stage);
            let _ = theirs.write_all(&match outcome {
                Ok(()) => [0, 0],
                Err((step, errno)) => [step.code(), errno_byte(errno)],
            });
            hatchling_sys::exit_child(i32::from(outcome.is_err()));
        }
        Ok(hatchling_sys::ForkResult::Parent { child }) => {
            drop(theirs);
            let outcome = cell_parent(&mut ours, child.pid().get(), &maps);
            drop(ours);
            let exited = child.wait_deadline(Duration::from_secs(30));
            match (outcome, exited) {
                (Ok(()), Ok(status)) if hatchling_sys::child_exit_code(status) == 0 => Ok(()),
                (Ok(()), _) => Err((Step::Ended, None)),
                (Err(failed), _) => Err(failed),
            }
        }
        Err(err) => Err((Step::Ended, errno(&err))),
    }
}

/// The probe's side outside the cell: waits for its user namespace,
/// writes its maps, and reads how the rest went.
fn cell_parent(
    socket: &mut UnixStream,
    pid: i32,
    (users, groups): &(hatchling_sys::IdMap, hatchling_sys::IdMap),
) -> Result<(), Failed> {
    let mut said = [0u8; 2];
    socket
        .read_exact(&mut said)
        .map_err(|_| (Step::Ended, None))?;
    let [code, number] = said;
    if code != 0 {
        return Err((Step::from_code(code), Some(i32::from(number))));
    }
    let mapped = hatchling_sys::idmap::write_maps(pid, users, groups);
    let _ = socket.write_all(&[u8::from(mapped.is_ok())]);
    mapped.map_err(|err| (Step::Maps, errno(&err)))?;
    socket
        .read_exact(&mut said)
        .map_err(|_| (Step::Ended, None))?;
    match said {
        [0, _] => Ok(()),
        [code, number] => Err((Step::from_code(code), Some(i32::from(number)))),
    }
}

/// The probe's side in the cell, run in the forked child of a process
/// with one thread.
fn cell_child(socket: &mut UnixStream, stage: &Path) -> Result<(), Failed> {
    hatchling_sys::unshare_user().map_err(|err| (Step::User, errno(&err)))?;
    let mut mapped = [0u8; 1];
    let _ = socket.write_all(&[0, 0]);
    if socket.read_exact(&mut mapped).is_err() || mapped != [1] {
        return Err((Step::Maps, None));
    }
    hatchling_sys::unshare_mount().map_err(|err| (Step::Mount, errno(&err)))?;
    hatchling_sys::unshare_net().map_err(|err| (Step::Net, errno(&err)))?;
    hatchling_sys::unshare_ipc().map_err(|err| (Step::Ipc, errno(&err)))?;
    std::fs::create_dir(stage).map_err(|err| (Step::Root, err.raw_os_error()))?;
    hatchling_sys::MountPlan::new(stage.to_path_buf(), &HashSet::new(), Vec::new())
        .and_then(|plan| plan.apply())
        .map_err(|err| (Step::Root, errno(&err)))?;
    hatchling_sys::unshare_pid().map_err(|err| (Step::Pid, errno(&err)))
}

/// What the kernel's settings say about making a user namespace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Restriction {
    /// Nothing they say stops one.
    #[default]
    None,
    /// `user.max_user_namespaces` is 0: nobody may make one.
    NoneAllowed,
    /// Ubuntu 23.10 and later: an unprivileged program needs an
    /// `AppArmor` profile that allows it.
    AppArmor,
    /// Debian's older kernels: only root may make one.
    RootOnly,
}

/// Who `doctor` runs as, and what the kernel allows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Facts {
    /// Whether `doctor` runs as root, as a service's broker does.
    root: bool,
    /// The kernel's restriction, the first of them that holds.
    restriction: Restriction,
}

impl Facts {
    fn read() -> Self {
        let says = |path: &str, value: &str| {
            std::fs::read_to_string(path).is_ok_and(|read| read.trim() == value)
        };
        let restriction = if says("/proc/sys/user/max_user_namespaces", "0") {
            Restriction::NoneAllowed
        } else if says(
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
            "1",
        ) {
            Restriction::AppArmor
        } else if says("/proc/sys/kernel/unprivileged_userns_clone", "0") {
            Restriction::RootOnly
        } else {
            Restriction::None
        };
        Self {
            root: hatchling_sys::euid() == 0,
            restriction,
        }
    }
}

/// Run on the host or a VM, where a container's policy is the cause.
const NOT_IN_A_CONTAINER: &str =
    "Usually a container's or a hardened kernel's policy: run Hatchling on the host, or in a VM.";

/// Install as a service, whose broker runs as root.
const AS_A_SERVICE: &str =
    "Install Hatchling as a service (docs/install.md): its broker runs as root";

/// Whether `errno` is a refusal of permission (`EPERM` or `EACCES`).
fn refused_permission(errno: Option<i32>) -> bool {
    errno.is_some_and(|errno| {
        std::io::Error::from_raw_os_error(errno).kind() == std::io::ErrorKind::PermissionDenied
    })
}

/// What went wrong making a cell at `step`, and the fix.
fn cell_advice(step: Step, errno: Option<i32>, facts: Facts) -> (String, String) {
    let because = errno.map_or_else(String::new, |errno| {
        format!(": {}", std::io::Error::from_raw_os_error(errno))
    });
    let refused = |kind: &str| {
        (
            format!(
                "this machine gives a cell its user namespace, but not a {kind} namespace{because}"
            ),
            String::from(NOT_IN_A_CONTAINER),
        )
    };
    match step {
        Step::User if facts.restriction == Restriction::NoneAllowed => (
            String::from("this kernel allows no user namespaces (user.max_user_namespaces is 0)"),
            String::from(
                "Allow them: `sudo sysctl -w user.max_user_namespaces=15000`, and set it in /etc/sysctl.d to keep it.",
            ),
        ),
        // Ubuntu lets the namespace be made, without the capabilities to
        // use it: the refusal comes at whichever step needs one.
        step if !facts.root
            && facts.restriction == Restriction::AppArmor
            && step != Step::Ended
            && (step == Step::User || refused_permission(errno)) =>
        {
            (
                String::from(
                    "AppArmor lets a program use a user namespace only with a profile that allows it (Ubuntu 23.10 and later), and the broker has none",
                ),
                format!(
                    "{AS_A_SERVICE}, or give the broker the profile in the README's \"Quickstart without root\"."
                ),
            )
        }
        Step::User if !facts.root && facts.restriction == Restriction::RootOnly => (
            String::from(
                "this kernel lets only root make a user namespace (kernel.unprivileged_userns_clone is 0)",
            ),
            format!(
                "{AS_A_SERVICE}, or allow them: `sudo sysctl -w kernel.unprivileged_userns_clone=1`."
            ),
        ),
        Step::User => (
            format!("this machine refused a cell its user namespace{because}"),
            String::from(NOT_IN_A_CONTAINER),
        ),
        Step::Maps => (
            format!("a cell's user couldn't be mapped into it{because}"),
            String::from(
                "As root, HATCHLING_AGENT_UID names the cells' user (the service's is hatchling-cell); without root, see the README's \"Running without root\".",
            ),
        ),
        Step::Mount => refused("mount"),
        Step::Net => refused("network"),
        Step::Ipc => refused("IPC"),
        Step::Pid => refused("pid"),
        Step::Root => (
            format!("a cell's own root filesystem couldn't be made{because}"),
            String::from(NOT_IN_A_CONTAINER),
        ),
        Step::Ended => (
            format!("the cell probe ended before saying how it went{because}"),
            String::from(
                "Run doctor again. If it says this again, report it with the output of `uname -a`.",
            ),
        ),
    }
}

/// `found`, the model check's finding, with the window the server at
/// `endpoint` gives the model `name` once it found it, when the server
/// says ([`crate::model_window`]).
fn with_window(found: Finding, endpoint: Option<&str>, name: Option<&str>) -> Finding {
    let (Finding::Fine(_), Some((authority, _)), Some(name)) =
        (&found, endpoint.and_then(plain_http), name)
    else {
        return found;
    };
    let loaded = crate::model_window::vouched(
        authority,
        name,
        crate::model_window::loaded_at(authority, name, CONNECT_TIMEOUT, ANSWER_TIMEOUT),
        CONNECT_TIMEOUT,
        ANSWER_TIMEOUT,
    );
    fitted(
        windowed(found, name, loaded.and_then(|loaded| loaded.window)),
        loaded,
    )
}

/// `found`, with how the loaded model fits, as its server says: all on
/// the GPU, or how much runs on the CPU, and what helps. A model on the
/// CPU still works, more slowly: never a problem.
fn fitted(found: Finding, loaded: Option<crate::model_window::Loaded>) -> Finding {
    use crate::model_window::gigabytes;
    let (Finding::Fine(seen), Some(loaded)) = (&found, loaded) else {
        return found;
    };
    if loaded.size == 0 {
        return found;
    }
    if loaded.on_cpu() == 0 {
        return Finding::Fine(format!(
            "{seen}, all on the GPU ({})",
            gigabytes(loaded.size)
        ));
    }
    Finding::Fine(format!(
        "{seen}, {} of its {} on the CPU, more slowly: free the GPU's memory, or give it a smaller window (OLLAMA_CONTEXT_LENGTH={})",
        gigabytes(loaded.on_cpu()),
        gigabytes(loaded.size),
        smaller_window(loaded.window)
    ))
}

/// A smaller window to try for a model that doesn't fit with `window`:
/// the recommended one, when it has more, else half of it, never under
/// the agent's floor.
fn smaller_window(window: Option<u32>) -> u32 {
    match window {
        Some(window) if window > RECOMMENDED_WINDOW => RECOMMENDED_WINDOW,
        Some(window) => (window / 2).max(crate::config::MIN_CONTEXT_WINDOW),
        None => RECOMMENDED_WINDOW / 2,
    }
}

/// The computer's memory: how much is free, and whether that's low.
fn check_memory(memory: Option<(u64, u64)>) -> Finding {
    use crate::model_window::{gigabytes, memory_low};
    let Some((available, total)) = memory else {
        return Finding::Unchecked(String::from("couldn't read /proc/meminfo"));
    };
    let free = format!("{} of {} free", gigabytes(available), gigabytes(total));
    if memory_low(available, total) {
        Finding::Fine(format!(
            "{free}: low. Other programs hold most of it, and Hatchling's answers slow down when it runs out"
        ))
    } else {
        Finding::Fine(free)
    }
}

/// The window Hatchling asks a model's server for.
///
/// Room for the agent's prompt, its tools, a working history and a
/// thinking model's answer. Less works, to the agent's floor, with less
/// of the conversation kept.
pub const RECOMMENDED_WINDOW: u32 = 32_768;

/// `found`, the model `name` found, with `window`, the window its server
/// gives it: one too small for an answer is a problem, and one under
/// [`RECOMMENDED_WINDOW`] says so.
fn windowed(found: Finding, name: &str, window: Option<u32>) -> Finding {
    match (found, window) {
        (_, Some(window)) if window < crate::config::MIN_CONTEXT_WINDOW => Finding::problem(
            format!(
                "the model's server gives {name} a {window}-token window, and one answer can take 6,000 tokens"
            ),
            "Give the model room: for Ollama, set OLLAMA_CONTEXT_LENGTH=32768 (docs/install.md, \"Give the model room\").",
        ),
        (Finding::Fine(seen), Some(window)) if window < RECOMMENDED_WINDOW => {
            Finding::Fine(format!(
                "{seen}, with a {window}-token window ({RECOMMENDED_WINDOW} or more works better)"
            ))
        }
        (Finding::Fine(seen), Some(window)) => {
            Finding::Fine(format!("{seen}, with a {window}-token window"))
        }
        (found, _) => found,
    }
}

/// The authority and path of the plain-HTTP model endpoint `endpoint`:
/// `None` for any other, a hosted one (`https`) among them.
#[must_use]
pub fn plain_http(endpoint: &str) -> Option<(&str, &str)> {
    endpoint
        .strip_prefix("http://")
        .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
        .filter(|(authority, _)| !authority.is_empty() && !authority.contains('@'))
}

/// Whether the model's server at `endpoint` answers, and has the model
/// `name`.
fn check_model(endpoint: Option<&str>, name: Option<&str>) -> Finding {
    let (Some(endpoint), Some(name)) = (endpoint, name) else {
        return Finding::problem(
            "no model is set: the agent has nothing to answer with",
            "Set HATCHLING_SCHEDULER_MODEL_ENDPOINT and HATCHLING_SCHEDULER_MODEL_NAME (docs/install.md).",
        );
    };
    if endpoint.starts_with("https://") {
        return Finding::Unchecked(format!(
            "{endpoint} is a hosted model, whose key only the broker reads"
        ));
    }
    let Some((authority, path)) = plain_http(endpoint) else {
        return Finding::problem(
            format!("can't read {endpoint} as a model's address"),
            "Set HATCHLING_SCHEDULER_MODEL_ENDPOINT to the API's base, such as http://localhost:11434/v1.",
        );
    };
    let models_path = format!("/{}/models", path.trim_end_matches('/'));
    let models_path = models_path.replace("//", "/");
    let answer = match ask(authority, &models_path) {
        Ok(answer) => answer,
        Err(err) => {
            return Finding::problem(
                format!("nothing answers at {authority}: {err}"),
                "Start the model's server: for Ollama, `ollama serve` (or `sudo systemctl start ollama`).",
            );
        }
    };
    let Some(ids) = model_ids(answer.as_deref()) else {
        return Finding::problem(
            format!("{authority} answers, but not with a list of models at {models_path}"),
            "Set HATCHLING_SCHEDULER_MODEL_ENDPOINT to an OpenAI-compatible API's base, such as http://localhost:11434/v1.",
        );
    };
    let latest = format!("{name}:latest");
    if ids.iter().any(|id| id == name || *id == latest) {
        return Finding::Fine(format!("{name} at {endpoint}"));
    }
    // The server's words, for a terminal: a control character in them
    // would be the terminal's command, not text.
    let has: Vec<String> = ids
        .iter()
        .take(MODELS_SHOWN)
        .map(|id| id.chars().flat_map(char::escape_debug).collect())
        .collect();
    let has = if has.is_empty() {
        String::from("none")
    } else {
        has.join(", ")
    };
    Finding::problem(
        format!("{endpoint} has no model {name} (it has: {has})"),
        format!(
            "Fetch it (for Ollama, `ollama pull {name}`), or set HATCHLING_SCHEDULER_MODEL_NAME to one it has."
        ),
    )
}

/// The answer to a plain-HTTP `GET path` at `authority`: its body when
/// its status is 200, `None` for any other.
fn ask(authority: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    ask_within(authority, path, CONNECT_TIMEOUT, ANSWER_TIMEOUT)
}

/// [`ask`], waiting `connect` to connect and `answer` for each read and
/// write: one bounded `GET` with no credential, its answer at most
/// [`MAX_ANSWER`] bytes.
///
/// # Errors
///
/// When nothing at `authority` connects, or the exchange fails.
pub fn ask_within(
    authority: &str,
    path: &str,
    connect: Duration,
    answer_within: Duration,
) -> Result<Option<Vec<u8>>, String> {
    let address = if authority.contains(':') && !authority.ends_with(']') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    let addresses = address.to_socket_addrs().map_err(|err| err.to_string())?;
    let mut last = String::from("no address");
    let mut stream = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, connect) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(err) => last = err.to_string(),
        }
    }
    let mut stream = stream.ok_or(last)?;
    stream
        .set_read_timeout(Some(answer_within))
        .and_then(|()| stream.set_write_timeout(Some(answer_within)))
        .and_then(|()| {
            // HTTP/1.0: the answer ends with the connection, never chunked.
            write!(
                stream,
                "GET {path} HTTP/1.0\r\nHost: {authority}\r\nAccept: application/json\r\n\r\n"
            )
        })
        .map_err(|err| err.to_string())?;
    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER)
        .read_to_end(&mut answer)
        .map_err(|err| err.to_string())?;
    Ok(body_if_ok(&answer))
}

/// The body of the HTTP answer `answer`, when its status is 200.
fn body_if_ok(answer: &[u8]) -> Option<Vec<u8>> {
    let end = answer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let (head, body) = answer.split_at(end);
    let status = head.split(|byte| *byte == b' ').nth(1)?;
    (status == b"200").then(|| body.get(4..).unwrap_or_default().to_vec())
}

/// The ids of an OpenAI-compatible list of models, `{"data": [{"id"}]}`.
#[must_use]
pub fn model_ids(answer: Option<&[u8]>) -> Option<Vec<String>> {
    let list: serde_json::Value = serde_json::from_slice(answer?).ok()?;
    Some(
        list.get("data")?
            .as_array()?
            .iter()
            .filter_map(|model| model.get("id")?.as_str().map(str::to_owned))
            .collect(),
    )
}

/// Prints the findings, read from `source`, and exits 1 if any is a
/// problem.
fn report(source: &str, checks: &[(&str, Finding)]) -> ExitCode {
    let (text, problems) = rendered(source, checks);
    let _ = write!(std::io::stdout().lock(), "{text}");
    if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The report, and how many problems it names.
fn rendered(source: &str, checks: &[(&str, Finding)]) -> (String, usize) {
    let mut text = format!("Hatchling doctor, with the settings in {source}:\n\n");
    let mut problems = 0usize;
    for (name, finding) in checks {
        let _ = match finding {
            Finding::Fine(seen) => writeln!(text, "  ok       {name:<14} {seen}"),
            Finding::Unchecked(why) => writeln!(text, "  skipped  {name:<14} {why}"),
            Finding::Problem { what, fix } => {
                problems = problems.saturating_add(1);
                writeln!(text, "  PROBLEM  {name:<14} {what}\n{:26}fix: {fix}", "")
            }
        };
    }
    let _ = match problems {
        0 => writeln!(text, "\nNothing found wrong."),
        1 => writeln!(text, "\n1 problem to fix."),
        many => writeln!(text, "\n{many} problems to fix."),
    };
    (text, problems)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    /// How the vault opens, in each case: by itself, or after each restart
    /// with the owner; and, waiting now, what to do.
    #[test]
    fn the_vault_says_how_it_opens_after_a_restart() {
        use hatchling_vault::Tier;
        let here = |keys: &[&str], wsl: bool| Machine {
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            wsl,
        };
        let fine = |finding: Finding| match finding {
            Finding::Fine(said) => said,
            other @ (Finding::Problem { .. } | Finding::Unchecked(_)) => {
                format!("not fine: {other:?}")
            }
        };
        let fix = |finding: Finding| match finding {
            Finding::Problem { what, fix } => format!("{what} | {fix}"),
            other @ (Finding::Fine(_) | Finding::Unchecked(_)) => {
                format!("not a problem: {other:?}")
            }
        };
        assert!(fine(vault_said(Tier::File, 0, &here(&[], false), false)).contains("by itself"));
        assert!(
            fine(vault_said(Tier::Totp, 0, &here(&[], false), false)).contains("authenticator app")
        );
        let two = fine(vault_said(
            Tier::HardwareKey,
            2,
            &here(&["hidraw0"], false),
            false,
        ));
        assert!(
            two.starts_with("opens with 2 security keys: after each restart"),
            "{two}"
        );
        assert!(
            !two.contains("Only one key") && !two.contains("None is"),
            "{two}"
        );
        let one = fine(vault_said(Tier::HardwareKey, 1, &here(&[], true), false));
        assert!(one.starts_with("opens with 1 security key:"), "{one}");
        assert!(one.contains("Only one key: add a second"), "{one}");
        assert!(one.contains("in WSL, attach it with usbipd"), "{one}");
        assert!(
            fine(vault_said(Tier::HardwareKey, 2, &here(&[], false), false))
                .contains("None is plugged in now")
        );
        // Waiting to be unlocked is the problem, with the way out.
        let waiting = fix(vault_said(Tier::HardwareKey, 2, &here(&[], true), true));
        assert!(waiting.starts_with("locked:"), "{waiting}");
        assert!(
            waiting.contains("usbipd attach --wsl")
                && waiting.contains("sudo hatchling-broker unlock"),
            "{waiting}"
        );
        let plugged = fix(vault_said(
            Tier::HardwareKey,
            2,
            &here(&["hidraw0"], true),
            true,
        ));
        assert!(
            !plugged.contains("usbipd") && plugged.contains("touch your security key"),
            "{plugged}"
        );
        assert!(
            fix(vault_said(Tier::Totp, 0, &here(&[], false), true)).contains("authenticator app")
        );
        // No keyring: its key file; one that doesn't load: the problem.
        let none: BTreeMap<String, String> = BTreeMap::new();
        assert!(
            fine(check_vault(
                &|key: &str| none.get(key).cloned(),
                &here(&[], false)
            ))
            .contains("key file")
        );
        let broken = BTreeMap::from([(
            String::from("HATCHLING_VAULT_KEYRING"),
            String::from("/nonexistent/hatchling/keyring"),
        )]);
        assert!(
            fix(check_vault(
                &|key: &str| broken.get(key).cloned(),
                &here(&[], false)
            ))
            .contains("doesn't load")
        );
    }

    /// The other model's server is one the app can switch to when egress
    /// grants it, to runs and to the chat; none set up is fine.
    #[test]
    fn the_other_server_is_checked_for_its_grants() {
        let other = |pairs: &[(&str, &str)]| {
            let map: BTreeMap<String, String> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            check_other(&|key: &str| map.get(key).cloned())
        };
        assert!(matches!(other(&[]), Finding::Fine(seen) if seen.starts_with("none set up")));
        let set_up = [
            (
                "HATCHLING_SCHEDULER_MODEL_OTHER_ENDPOINT",
                "https://openrouter.ai/api/v1",
            ),
            ("HATCHLING_SCHEDULER_MODEL_OTHER_NAME", "nemotron"),
        ];
        let with = |more: &[(&'static str, &'static str)]| {
            let mut pairs = set_up.to_vec();
            pairs.extend_from_slice(more);
            other(&pairs)
        };
        assert!(matches!(
            with(&[]),
            Finding::Problem { what, .. } if what.contains("egress doesn't grant openrouter.ai:443")
        ));
        let egress = (
            "HATCHLING_EGRESS_GRANTS",
            "localhost:11434, openrouter.ai:443",
        );
        assert!(
            matches!(with(&[egress]), Finding::Fine(seen) if seen.contains("switch to it in the app"))
        );
        // With a chat, its grants must reach it too.
        assert!(matches!(
            with(&[
                egress,
                (
                    "HATCHLING_API_GRANTS",
                    "channel-send:local,egress:localhost:11434"
                )
            ]),
            Finding::Problem { .. }
        ));
        assert!(matches!(
            with(&[
                egress,
                (
                    "HATCHLING_API_GRANTS",
                    "channel-send:local,egress:openrouter.ai:443"
                )
            ]),
            Finding::Fine(_)
        ));
        assert!(matches!(
            other(&[
                ("HATCHLING_SCHEDULER_MODEL_OTHER_ENDPOINT", "ftp://models.example/v1"),
                ("HATCHLING_SCHEDULER_MODEL_OTHER_NAME", "m"),
            ]),
            Finding::Problem { what, .. } if what.contains("can't read")
        ));
    }
    use crate::test_port::RefusingPort;

    /// What `found` says is wrong, and the fix: it must be a problem.
    fn problem(found: Finding) -> (String, String) {
        match found {
            Finding::Problem { what, fix } => (what, fix),
            other @ (Finding::Fine(_) | Finding::Unchecked(_)) => {
                assert_eq!(other, Finding::problem("", ""), "not a problem");
                (String::new(), String::new())
            }
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-doctor-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_come_from_the_named_file_the_environment_or_the_service() {
        let dir = scratch("source");
        let service = dir.join("hatchling.env");
        let named = args(&["--env", "/etc/other.env"]);
        // No service file: the environment, set or not.
        assert_eq!(source(&[], false, &service), Source::Environment);
        std::fs::write(&service, "").unwrap();
        assert_eq!(source(&[], false, &service), Source::File(service.clone()));
        assert_eq!(source(&[], true, &service), Source::Environment);
        for set in [false, true] {
            assert_eq!(
                source(&named, set, &service),
                Source::File("/etc/other.env".into())
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_settings_file_reads_as_systemd_reads_it() {
        let read = parse_settings(
            "# a comment\n; another\n\n  HATCHLING_A=one\nHATCHLING_B = \"two words\"\nHATCHLING_C='three'\nHATCHLING_D=a=b\nno value here\nHATCHLING_E=\"unclosed\n",
        );
        let expected: BTreeMap<String, String> = [
            ("HATCHLING_A", "one"),
            ("HATCHLING_B", "two words"),
            ("HATCHLING_C", "three"),
            ("HATCHLING_D", "a=b"),
            ("HATCHLING_E", "\"unclosed"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(read, expected);
    }

    #[test]
    fn settings_that_dont_parse_are_named() {
        let (what, _) = problem(check_settings(&|_: &str| None));
        assert!(what.contains("HATCHLING_BROKER_SOCKET"), "{what}");
    }

    #[test]
    fn the_agent_program_must_be_a_program() {
        let dir = scratch("program");
        let program = dir.join("hatchling-agent");
        let missing = program.to_str().unwrap();
        assert!(matches!(check_program(None), Finding::Problem { .. }));
        assert!(matches!(
            check_program(Some(missing)),
            Finding::Problem { .. }
        ));
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            check_program(Some(missing)),
            Finding::Problem { .. }
        ));
        assert!(matches!(
            check_program(dir.to_str()),
            Finding::Problem { .. }
        ));
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            check_program(Some(missing)),
            Finding::Fine(missing.to_owned())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_cell_probe_says_whether_a_cell_can_be_made_and_leaves_nothing() {
        // The machine's own answer, either way: it must be fine where
        // cells are required to work, and a named problem where not.
        // As root, the cells' own user, as a service's; else one's own.
        let agent = if hatchling_sys::euid() == 0 {
            String::from("hatchling-cell")
        } else {
            hatchling_sys::euid().to_string()
        };
        let uid = crate::config::agent_uid_value(&agent).ok();
        let lookup = |key: &str| (key == env::HATCHLING_AGENT_UID.name).then(|| agent.clone());
        // Shared, as the temp directory is: the probe makes its directory
        // as the cells' user.
        let temp = scratch("probe-temp");
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let found = check_cells_in(&lookup, &temp);
        let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|value| value == "1");
        match &found {
            Finding::Fine(seen) => {
                assert_eq!(seen, &format!("a cell runs as uid {}", uid.unwrap()));
            }
            Finding::Problem { what, fix } => {
                assert!(!required, "cells are required: {what}");
                assert!(!what.is_empty() && !fix.is_empty());
            }
            Finding::Unchecked(why) => assert_eq!(why, "", "cells are always checked"),
        }
        // Its staging directory, randomly named, is gone.
        let left: Vec<_> = std::fs::read_dir(&temp)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert!(left.is_empty(), "the probe left {left:?}");
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn a_cell_the_kernel_refuses_is_named_by_its_step() {
        // Without root, a process maps only its own uid: another's map is
        // refused, wherever namespaces work.
        let euid = hatchling_sys::euid();
        if euid == 0 {
            return;
        }
        let stage =
            std::env::temp_dir().join(format!("hatchling-doctor-refused-{}", std::process::id()));
        let refused = probe_cell(&CellIdentity::direct(euid.saturating_add(1), 1), &stage);
        assert!(!stage.exists());
        let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|value| value == "1");
        if required {
            assert_eq!(refused, Err((Step::Maps, Some(1))));
        } else {
            assert!(refused.is_err());
        }
    }

    #[test]
    fn each_cell_step_has_its_own_advice() {
        let facts = |root, restriction| Facts { root, restriction };
        let user = facts(false, Restriction::None);
        let what = cell_advice;
        let (said, fix) = what(Step::User, Some(1), facts(false, Restriction::AppArmor));
        assert!(
            said.contains("AppArmor") && fix.contains("Quickstart without root"),
            "{said}"
        );
        // Ubuntu 24.04 makes the namespace, then refuses its first use:
        // a refusal of permission at any step is AppArmor's.
        for step in [
            Step::Maps,
            Step::Mount,
            Step::Net,
            Step::Ipc,
            Step::Root,
            Step::Pid,
        ] {
            for errno in [1, 13] {
                let (said, _) = what(step, Some(errno), facts(false, Restriction::AppArmor));
                assert!(said.starts_with("AppArmor"), "{step:?} {errno}: {said}");
            }
            // Another failure is the step's own.
            let (said, _) = what(step, Some(28), facts(false, Restriction::AppArmor));
            assert!(!said.starts_with("AppArmor"), "{step:?}: {said}");
        }
        let (said, _) = what(Step::Mount, None, facts(false, Restriction::AppArmor));
        assert!(said.ends_with("not a mount namespace"), "{said}");
        let (said, _) = what(Step::Ended, Some(1), facts(false, Restriction::AppArmor));
        assert!(said.starts_with("the cell probe ended"), "{said}");
        // Root is let through AppArmor: that isn't the cause there.
        let (said, _) = what(Step::Mount, Some(1), facts(true, Restriction::AppArmor));
        assert!(said.contains("not a mount namespace"), "{said}");
        let (said, fix) = what(Step::User, Some(1), facts(true, Restriction::AppArmor));
        assert!(
            said.ends_with(": Operation not permitted (os error 1)"),
            "{said}"
        );
        assert_eq!(fix, NOT_IN_A_CONTAINER);
        // Nor root, where none is allowed.
        for root in [false, true] {
            let (said, fix) = what(Step::User, None, facts(root, Restriction::NoneAllowed));
            assert!(
                said.contains("max_user_namespaces") && fix.contains("sysctl"),
                "{said}"
            );
        }
        let (said, fix) = what(Step::User, None, facts(false, Restriction::RootOnly));
        assert!(said.contains("unprivileged_userns_clone") && fix.starts_with(AS_A_SERVICE));
        let (said, _) = what(Step::User, None, facts(true, Restriction::RootOnly));
        assert!(!said.contains("unprivileged_userns_clone"), "{said}");
        let (said, fix) = what(Step::User, Some(1), user);
        assert!(
            said.starts_with("this machine refused a cell its user namespace: "),
            "{said}"
        );
        assert_eq!(fix, NOT_IN_A_CONTAINER);
        assert!(what(Step::Maps, None, user).1.contains("hatchling-cell"));
        for (step, kind) in [
            (Step::Mount, "mount"),
            (Step::Net, "network"),
            (Step::Ipc, "IPC"),
            (Step::Pid, "pid"),
        ] {
            let (said, fix) = what(step, None, user);
            assert!(said.ends_with(&format!("not a {kind} namespace")), "{said}");
            assert_eq!(fix, NOT_IN_A_CONTAINER);
        }
        assert!(what(Step::Root, None, user).0.contains("root filesystem"));
        assert!(what(Step::Ended, None, user).1.contains("uname -a"));
    }

    #[test]
    fn step_codes_go_both_ways() {
        for step in Step::ALL {
            assert_ne!(step.code(), 0);
            assert_eq!(Step::from_code(step.code()), step);
        }
        assert_eq!(Step::from_code(200), Step::Ended);
        assert_eq!(errno_byte(Some(13)), 13);
        assert_eq!(errno_byte(Some(300)), 0);
        assert_eq!(errno_byte(None), 0);
    }

    /// A server at a free local port that answers one request with
    /// `answer`, and the request it got.
    ///
    /// It waits at most 10 seconds for the request: one that never comes
    /// fails the test, rather than hanging it.
    fn server(answer: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let served = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => return format!("no request: {err}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                request.extend_from_slice(&byte);
            }
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, served)
    }

    const MODELS: &str = "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"object\":\"list\",\"data\":[{\"id\":\"llama3.2:latest\"},{\"id\":\"qwen\\u001b[2J\"}]}";

    #[test]
    fn the_model_is_found_by_its_name_or_its_latest_tag() {
        for name in ["llama3.2", "llama3.2:latest"] {
            let (address, served) = server(MODELS);
            let endpoint = format!("http://{address}/v1/");
            assert_eq!(
                check_model(Some(&endpoint), Some(name)),
                Finding::Fine(format!("{name} at {endpoint}"))
            );
            let request = served.join().unwrap();
            assert!(
                request.starts_with(&format!("GET /v1/models HTTP/1.0\r\nHost: {address}\r\n")),
                "{request}"
            );
        }
    }

    #[test]
    fn a_missing_model_is_named_with_what_the_server_has() {
        let (address, served) = server(MODELS);
        let (what, fix) = problem(check_model(
            Some(&format!("http://{address}")),
            Some("mistral"),
        ));
        served.join().unwrap();
        // The server's escape reaches the terminal as text.
        assert!(
            what.ends_with("has no model mistral (it has: llama3.2:latest, qwen\\u{1b}[2J)"),
            "{what}"
        );
        assert!(!what.contains('\u{1b}'));
        assert!(fix.contains("ollama pull mistral"));
    }

    #[test]
    fn a_server_that_isnt_a_model_api_is_a_problem() {
        for answer in [
            "HTTP/1.0 404 Not Found\r\n\r\n{\"data\":[]}",
            "HTTP/1.0 200 OK\r\n\r\nnot json",
            "HTTP/1.0 200 OK\r\n\r\n{\"models\":[]}",
            "no head at all",
        ] {
            let (address, served) = server(answer);
            let found = check_model(Some(&format!("http://{address}/v1")), Some("llama3.2"));
            served.join().unwrap();
            let (what, _) = problem(found);
            assert!(
                what.contains("not with a list of models at /v1/models"),
                "{what}"
            );
        }
    }

    /// Once the model is found, Ollama is asked the window it gives it:
    /// one big enough is shown, and one too small is a problem with its
    /// fix. A model not found, or a hosted one, isn't asked about.
    #[test]
    fn a_found_models_window_is_shown_and_too_small_is_a_problem() {
        use crate::model_window::{answering, loaded};
        for (window, seen) in [(32768, true), (4096, false)] {
            let ps = loaded(&format!(
                r#"{{"name":"llama3.2:latest","context_length":{window}}}"#
            ));
            let (address, served) = answering(vec![String::from(MODELS), ps]);
            let endpoint = format!("http://{address}/v1");
            let found = with_window(
                check_model(Some(&endpoint), Some("llama3.2")),
                Some(&endpoint),
                Some("llama3.2"),
            );
            let heads = served.join().unwrap();
            assert!(
                heads
                    .get(1)
                    .is_some_and(|head| head.starts_with("GET /api/ps HTTP/1.0")),
                "{heads:?}"
            );
            if seen {
                assert_eq!(
                    found,
                    Finding::Fine(format!("llama3.2 at {endpoint}, with a 32768-token window"))
                );
            } else {
                let (what, fix) = problem(found);
                assert!(
                    what.ends_with(
                        "gives llama3.2 a 4096-token window, and one answer can take 6,000 tokens"
                    ),
                    "{what}"
                );
                assert!(fix.contains("OLLAMA_CONTEXT_LENGTH=32768"), "{fix}");
            }
        }
        let (address, served) = answering(vec![String::from(MODELS), loaded("")]);
        let endpoint = format!("http://{address}/v1");
        let missing = with_window(
            check_model(Some(&endpoint), Some("mistral")),
            Some(&endpoint),
            Some("mistral"),
        );
        assert!(matches!(missing, Finding::Problem { .. }));
        // The window's answer is still there for the next asker: doctor
        // didn't ask for it.
        assert_eq!(
            ask(&address, "/mine").unwrap(),
            Some(b"{\"models\":[]}".to_vec())
        );
        let heads = served.join().unwrap();
        assert!(
            heads
                .get(1)
                .is_some_and(|head| head.starts_with("GET /mine ")),
            "a missing model isn't asked about: {heads:?}"
        );
        let hosted = Finding::Unchecked(String::from("hosted"));
        assert_eq!(
            with_window(hosted.clone(), Some("https://api.example/v1"), Some("m")),
            hosted
        );
    }

    /// A loaded model's fit joins a model found: all on the GPU, or how
    /// much is on the CPU and what helps; a server that doesn't say adds
    /// nothing.
    #[test]
    fn a_found_models_fit_is_said() {
        use crate::model_window::Loaded;
        let fine = Finding::Fine(String::from("m at http://h/v1, with a 32768-token window"));
        let loaded = |size, on_gpu| {
            Some(Loaded {
                window: Some(32768),
                size,
                on_gpu,
            })
        };
        assert_eq!(
            fitted(fine.clone(), loaded(6_600_000_000, 6_600_000_000)),
            Finding::Fine(String::from(
                "m at http://h/v1, with a 32768-token window, all on the GPU (6.6 GB)"
            ))
        );
        assert_eq!(
            fitted(fine.clone(), loaded(10_700_000_000, 9_800_000_000)),
            Finding::Fine(String::from(
                "m at http://h/v1, with a 32768-token window, 0.9 GB of its 10.7 GB on the CPU, more slowly: free the GPU's memory, or give it a smaller window (OLLAMA_CONTEXT_LENGTH=16384)"
            ))
        );
        assert_eq!(fitted(fine.clone(), None), fine);
        assert_eq!(fitted(fine.clone(), loaded(0, 0)), fine, "no size, no fit");
        let missing = Finding::problem("missing", "fetch it");
        assert_eq!(fitted(missing.clone(), loaded(1, 0)), missing);
    }

    /// The smaller window to try is the recommended one when the model
    /// has more, else half, never under the agent's floor.
    #[test]
    fn a_smaller_window_is_one_that_helps() {
        assert_eq!(smaller_window(Some(131_072)), 32_768);
        assert_eq!(smaller_window(Some(32_769)), 32_768);
        assert_eq!(smaller_window(Some(32_768)), 16_384);
        assert_eq!(smaller_window(Some(16_384)), 8_192);
        assert_eq!(smaller_window(Some(12_000)), 8_192, "never under the floor");
        assert_eq!(smaller_window(None), 16_384);
    }

    /// The computer's memory is said, and low memory named.
    #[test]
    fn memory_is_said_and_low_named() {
        assert_eq!(
            check_memory(Some((12_300_000_000, 32_000_000_000))),
            Finding::Fine(String::from("12.3 GB of 32.0 GB free"))
        );
        let low = check_memory(Some((2_000_000_000, 32_000_000_000)));
        assert!(
            matches!(&low, Finding::Fine(seen) if seen.starts_with("2.0 GB of 32.0 GB free: low.")),
            "low memory is said, and is no problem: {low:?}"
        );
        assert!(matches!(check_memory(None), Finding::Unchecked(_)));
    }

    /// The window joins a model found, and nothing else; when the server
    /// doesn't say, the finding stands.
    #[test]
    fn a_window_joins_only_a_found_model() {
        let fine = Finding::Fine(String::from("m at http://h/v1"));
        assert_eq!(windowed(fine.clone(), "m", None), fine);
        assert_eq!(
            windowed(fine.clone(), "m", Some(8192)),
            Finding::Fine(String::from(
                "m at http://h/v1, with a 8192-token window (32768 or more works better)"
            ))
        );
        assert_eq!(
            windowed(fine.clone(), "m", Some(32767)),
            Finding::Fine(String::from(
                "m at http://h/v1, with a 32767-token window (32768 or more works better)"
            ))
        );
        assert_eq!(
            windowed(fine, "m", Some(32768)),
            Finding::Fine(String::from("m at http://h/v1, with a 32768-token window"))
        );
        let missing = Finding::problem("missing", "fetch it");
        assert_eq!(windowed(missing.clone(), "m", Some(16384)), missing);
        let (what, _) = problem(windowed(Finding::Fine(String::new()), "m", Some(8191)));
        assert!(what.contains("a 8191-token window"), "{what}");
    }

    #[test]
    fn no_model_server_unset_or_unreadable_is_a_problem_and_hosted_isnt_reached() {
        let refusing = RefusingPort::new();
        let address = refusing.addr();
        let (what, fix) = problem(check_model(
            Some(&format!("http://{address}/v1")),
            Some("llama3.2"),
        ));
        assert!(
            what.starts_with(&format!("nothing answers at {address}: ")),
            "{what}"
        );
        assert!(fix.contains("ollama serve"));
        for (endpoint, name) in [
            (None, Some("llama3.2")),
            (Some("http://localhost:1/v1"), None),
        ] {
            let (what, _) = problem(check_model(endpoint, name));
            assert!(what.starts_with("no model is set"));
        }
        for endpoint in ["localhost:11434/v1", "http://", "http://user@localhost/v1"] {
            let (what, _) = problem(check_model(Some(endpoint), Some("x")));
            assert!(what.starts_with("can't read"), "{what}");
        }
        assert!(matches!(
            check_model(Some("https://api.example/v1"), Some("x")),
            Finding::Unchecked(_)
        ));
    }

    #[test]
    fn a_status_is_read_from_the_head_only() {
        assert_eq!(
            body_if_ok(b"HTTP/1.1 200 OK\r\n\r\nbody"),
            Some(b"body".to_vec())
        );
        assert_eq!(body_if_ok(b"HTTP/1.1 200 OK\r\n\r\n"), Some(Vec::new()));
        assert_eq!(body_if_ok(b"HTTP/1.1 500 Error\r\n\r\n200"), None);
        assert_eq!(body_if_ok(b"HTTP/1.1 200 OK\r\n"), None);
    }

    #[test]
    fn the_report_counts_its_problems_and_shows_each_fix() {
        let fine = ("settings", Finding::Fine(String::from("they parse")));
        let skipped = ("model", Finding::Unchecked(String::from("hosted")));
        let problem = ("cells", Finding::problem("refused", "run elsewhere"));
        let (text, problems) = rendered("/etc/h.env", &[fine.clone(), skipped.clone()]);
        assert_eq!(problems, 0);
        assert_eq!(
            text,
            "Hatchling doctor, with the settings in /etc/h.env:\n\n  ok       settings       they parse\n  skipped  model          hosted\n\nNothing found wrong.\n"
        );
        let (text, problems) = rendered("x", &[fine, problem.clone()]);
        assert_eq!(problems, 1);
        let shown = format!(
            "  PROBLEM  cells          refused\n{}fix: run elsewhere\n",
            " ".repeat(26)
        );
        assert!(text.contains(&shown), "{text}");
        assert!(text.ends_with("\n1 problem to fix.\n"));
        let (text, problems) = rendered("x", &[problem.clone(), problem]);
        assert_eq!(problems, 2);
        assert!(text.ends_with("\n2 problems to fix.\n"));
        assert_eq!(report("x", &[skipped]), ExitCode::SUCCESS);
    }
}
