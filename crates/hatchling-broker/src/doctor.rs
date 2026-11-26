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

mod stores;

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
    /// Worth knowing, and not a problem: Hatchling works without it.
    Note {
        /// What there is to know.
        what: String,
        /// What to do, if it's wanted.
        fix: String,
    },
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
    let service = matches!(&source, Source::File(path) if path == Path::new(SERVICE_SETTINGS));
    let cgroup = lookup(env::HATCHLING_CELL_CGROUP.name).or_else(service_cell_cgroup);
    let checks = [
        ("settings", check_settings(&lookup)),
        (
            "agent program",
            check_program(lookup(env::HATCHLING_AGENT_BIN.name).as_deref()),
        ),
        ("cells", check_cells(&lookup)),
        ("cell limits", check_cell_limits(cgroup.as_deref())),
        (
            "browser",
            browser_said(
                is_set(lookup(env::HATCHLING_BROWSER.name)),
                cgroup.is_some(),
            ),
        ),
        ("agent uid", nobody_said(&lookup)),
        ("model", {
            let endpoint = lookup(env::HATCHLING_SCHEDULER_MODEL_ENDPOINT.name);
            let name = lookup(env::HATCHLING_SCHEDULER_MODEL_NAME.name);
            let found = check_model(endpoint.as_deref(), name.as_deref());
            with_window(found, endpoint.as_deref(), name.as_deref())
        }),
        ("other model", check_other(&lookup)),
        ("vault", check_vault(&lookup, &Machine::here())),
        ("stores", check_stores(&lookup)),
        (
            "confinement",
            confinement_said(hatchling_sys::landlock::landlock_abi()),
        ),
        (
            "users",
            users_said(service, hatchling_sys::euid(), &|name| {
                hatchling_sys::subid::local_account(name).map_err(|e| e.to_string())
            }),
        ),
        ("output gate", output_gate_said(&lookup)),
        ("decisions", decisions_said(&lookup)),
        ("room disk", check_room_disk(&lookup)),
        (
            "the Room",
            room_image_said(room_missing([
                (cfg!(target_arch = "x86_64"), RoomMissing::Arch),
                (
                    crate::room_launch::read_admission(Path::new(crate::room_launch::RECORD))
                        .is_ok(),
                    RoomMissing::Admission,
                ),
                (
                    Path::new(crate::room_launch::IMAGE_DIR).is_dir(),
                    RoomMissing::Image,
                ),
                (cgroup.is_some(), RoomMissing::Cgroup),
            ])),
        ),
        ("windows seal", wsl_seal_said(WslSeal::here())),
        (
            "keep running",
            keep_running_said(
                crate::windows_setup::installed(),
                crate::windows_setup::keep_running(Path::new(crate::windows_setup::KEEP_RUNNING)),
            ),
        ),
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

/// `HATCHLING_CELL_CGROUP` in a unit's `Environment=`, which is where the
/// service's cgroup drop-in sets it: it isn't in the settings file.
fn cell_cgroup_in(environment: &str) -> Option<String> {
    environment
        .split_whitespace()
        .find_map(|setting| setting.strip_prefix("HATCHLING_CELL_CGROUP="))
        .map(str::to_owned)
}

/// The cgroup the service's unit gives its cells, if it does.
pub(crate) fn service_cell_cgroup() -> Option<String> {
    let shown = std::process::Command::new("systemctl")
        .args(["show", "hatchling", "-p", "Environment", "--value"])
        .output()
        .ok()?;
    cell_cgroup_in(&String::from_utf8_lossy(&shown.stdout))
}

/// What's wrong with the delegated cgroup at `parent`, if anything, and
/// the fix: a broker that can't bound its cells stops at once, so this is
/// worth saying before it's started, and before anyone unlocks it.
///
/// It's what can be seen without changing anything
/// ([`hatchling_sys::cgroup::delegation_problem`]). The service's cgroup
/// isn't there before the service's first start, nor between the starts
/// of one that keeps stopping, so one that isn't there is judged by the
/// tree it will be made in.
pub(crate) fn cell_limits_problem(parent: Option<&str>) -> Option<(String, &'static str)> {
    let parent = parent?;
    hatchling_sys::cgroup::delegation_problem(Path::new(parent))
        .map(|problem| cell_limits_said(parent, &problem))
}

/// What became of the delegated cgroup, looked at by something that may
/// put it right ([`cell_limits_restored`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellLimits {
    /// Nothing is wrong, or cells aren't given a cgroup at all.
    Fine,
    /// It was short of these controllers, and has them back.
    Restored(Vec<&'static str>),
    /// Something is wrong still: what, and the fix.
    Problem(String, &'static str),
}

/// [`cell_limits_problem`], as what became of it: only looked at, and
/// nothing put right.
pub(crate) fn cell_limits_seen(parent: Option<&str>) -> CellLimits {
    cell_limits_problem(parent).map_or(CellLimits::Fine, |(what, fix)| {
        CellLimits::Problem(what, fix)
    })
}

/// How often the cgroup is looked at again once systemd was asked to
/// apply the unit's settings, and how long between: systemd may do it a
/// moment after it says it will.
const REAPPLIED_LOOKS: u32 = 20;
const REAPPLIED_PAUSE: Duration = Duration::from_millis(100);

/// [`cell_limits_problem`], having first put right what can be.
///
/// A service's cgroup that is there, and short of a controller its tree
/// offers, was left so by something outside Hatchling: under WSL, another
/// distro starting does it, since every distro shares one cgroup tree.
/// The unit asks for those controllers itself, so having systemd apply
/// its settings again gives them back, and nothing is restarted.
///
/// Only `unlock` calls this, as root, at its owner's word. `doctor` only
/// looks.
pub(crate) fn cell_limits_restored(parent: Option<&str>) -> CellLimits {
    let Some(parent) = parent else {
        return CellLimits::Fine;
    };
    let look = || hatchling_sys::cgroup::delegation_problem(Path::new(parent));
    let (left, back) = restored(
        look(),
        || ran("systemctl", "daemon-reload"),
        || {
            settled(
                look,
                || std::thread::sleep(REAPPLIED_PAUSE),
                REAPPLIED_LOOKS,
            )
        },
    );
    became(parent, left, back)
}

/// What became of the cgroup at `parent`: `left` is what is wrong with
/// it still, and `back` the controllers it was given back.
fn became(
    parent: &str,
    left: Option<hatchling_sys::cgroup::DelegationProblem>,
    back: Vec<&'static str>,
) -> CellLimits {
    match left {
        Some(problem) => {
            let (what, fix) = cell_limits_said(parent, &problem);
            CellLimits::Problem(what, fix)
        }
        None if back.is_empty() => CellLimits::Fine,
        None => CellLimits::Restored(back),
    }
}

/// What is left wrong of `first` once what can be put right has been, and
/// the controllers that were given back.
///
/// Only a cgroup that is there and lacks a controller is tried: `reapply`
/// has systemd apply the unit's settings again, and `again` looks after
/// it. Any other problem is left as it was found, and nothing is run.
fn restored(
    first: Option<hatchling_sys::cgroup::DelegationProblem>,
    reapply: impl FnOnce() -> bool,
    again: impl FnOnce() -> Option<hatchling_sys::cgroup::DelegationProblem>,
) -> (
    Option<hatchling_sys::cgroup::DelegationProblem>,
    Vec<&'static str>,
) {
    use hatchling_sys::cgroup::DelegationProblem;
    match first {
        Some(DelegationProblem::ParentLacks(missing)) => {
            if !reapply() {
                return (Some(DelegationProblem::ParentLacks(missing)), Vec::new());
            }
            match again() {
                None => (None, missing),
                still => (still, Vec::new()),
            }
        }
        other => (other, Vec::new()),
    }
}

/// Looks up to `looks` times, pausing between two looks, until nothing
/// is wrong: what the last look found.
fn settled<P>(
    mut look: impl FnMut() -> Option<P>,
    mut pause: impl FnMut(),
    looks: u32,
) -> Option<P> {
    let mut found = look();
    for _ in 1..looks {
        if found.is_none() {
            break;
        }
        pause();
        found = look();
    }
    found
}

/// Runs `program` with the one argument, and says whether it ended well.
fn ran(program: &str, argument: &str) -> bool {
    std::process::Command::new(program)
        .arg(argument)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// What's said of `problem` with the cell cgroup at `parent`, and the fix.
fn cell_limits_said(
    parent: &str,
    problem: &hatchling_sys::cgroup::DelegationProblem,
) -> (String, &'static str) {
    use hatchling_sys::cgroup::DelegationProblem;
    match problem {
        DelegationProblem::ParentLacks(missing) => (
            format!(
                "the service's cgroup has no {} controller, and cells won't run without one",
                missing.join(", ")
            ),
            CELL_LIMITS_FIX,
        ),
        DelegationProblem::TreeLacks(missing) => (
            format!(
                "this machine's cgroups offer no {} controller, so cells can't have a cgroup of their own, and with HATCHLING_CELL_CGROUP set the broker stops at every start",
                missing.join(", ")
            ),
            NO_CONTROLLERS_FIX,
        ),
        DelegationProblem::NotCgroup2 => (
            format!(
                "HATCHLING_CELL_CGROUP ({parent}) isn't in a cgroup v2 tree, and the broker stops at every start"
            ),
            NOT_A_CGROUP_FIX,
        ),
    }
}

/// How to get the controllers back: a reload has systemd apply them again.
pub(crate) const CELL_LIMITS_FIX: &str =
    "Run `sudo systemctl daemon-reload`, then `sudo systemctl restart hatchling`.";

/// What to do on a machine whose cgroups offer no controller cells need.
const NO_CONTROLLERS_FIX: &str = "Run the installer again: on a machine like this it holds cells by process limits alone, and says so. Or do it by hand: remove /etc/systemd/system/hatchling.service.d/cgroup.conf, then `sudo systemctl daemon-reload` and `sudo systemctl restart hatchling`.";

/// What to do about a cell cgroup that isn't one.
const NOT_A_CGROUP_FIX: &str = "Set it to the service's own cgroup under /sys/fs/cgroup, or unset it to hold cells by process limits alone (README, Cell limits).";

/// What doctor says of cells with no cgroup of their own: it works, and
/// it's weaker, so it's said as something to know.
const PROCESS_LIMITS: &str = "cells are held by process limits alone (HATCHLING_CELL_CGROUP isn't set): nothing bounds a cell's memory or CPU as a whole, a cell that ends isn't killed as one, and the agent's computer (the Room) and its browser can't run, as each needs a cgroup";

/// How to give cells a cgroup, where the machine can.
const PROCESS_LIMITS_FIX: &str = "where this machine's cgroups offer the pids, memory and cpu controllers (`cat /sys/fs/cgroup/cgroup.controllers`), run the installer again, or see the README's Cell limits";

/// Whether cells can be bounded as the settings say.
fn check_cell_limits(parent: Option<&str>) -> Finding {
    cell_limits_found(parent.is_some(), cell_limits_problem(parent))
}

/// What doctor finds of cell limits: with a cgroup `set` or not, and the
/// `problem` with it.
fn cell_limits_found(set: bool, problem: Option<(String, &'static str)>) -> Finding {
    match (set, problem) {
        (_, Some((what, fix))) => Finding::problem(what, fix),
        (true, None) => Finding::Fine(String::from("each cell has its own cgroup")),
        (false, None) => Finding::Note {
            what: String::from(PROCESS_LIMITS),
            fix: String::from(PROCESS_LIMITS_FIX),
        },
    }
}

/// What stops the agent's computer, the Room, starting here (WO-037).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomMissing {
    /// Its supervisor needs `x86_64`.
    Arch,
    /// No image is admitted (`/etc/hatchling/room-image.admission`).
    Admission,
    /// Its image isn't in place.
    Image,
    /// Cells have no cgroup, which it needs.
    Cgroup,
}

impl RoomMissing {
    /// What the owner is told of it.
    const fn said(self) -> &'static str {
        match self {
            Self::Arch => "it needs x86_64 Linux",
            Self::Admission => "no image is installed (/etc/hatchling/room-image.admission)",
            Self::Image => "its image isn't in place (/usr/local/share/hatchling/room)",
            Self::Cgroup => "cells are held by process limits alone, and it needs a cgroup",
        }
    }

    /// What the owner can do about it (#584): the installer adds the image
    /// on Linux as Windows Setup does on Windows, and neither helps a
    /// machine that isn't `x86_64` or has no cgroup for cells.
    const fn fix(self) -> &'static str {
        match self {
            Self::Arch => {
                "run Hatchling on an x86_64 machine; everything else works here without it"
            }
            Self::Admission | Self::Image => {
                "install a release that has its image, on x86_64: the installer adds it on Linux (docs/install.md), Windows Setup on Windows (docs/windows.md)"
            }
            Self::Cgroup => "give cells a cgroup, as the cell limits line above says",
        }
    }
}

/// The first of `checks` that fails, in order: what stops the Room.
fn room_missing(checks: [(bool, RoomMissing); 4]) -> Option<RoomMissing> {
    checks
        .into_iter()
        .find(|(passed, _)| !passed)
        .map(|(_, missing)| missing)
}

/// What doctor says of the agent's computer: whether it can start here, and
/// what's `missing` if it can't. Read, never started.
fn room_image_said(missing: Option<RoomMissing>) -> Finding {
    missing.map_or_else(
        || {
            Finding::Fine(String::from(
                "its image is installed: it starts when a run asks for it",
            ))
        },
        |missing| Finding::Note {
            what: format!(
                "the agent's computer (the Room) can't start here: {}",
                missing.said()
            ),
            fix: String::from(missing.fix()),
        },
    )
}

/// What doctor reads of a WSL distro's seal from Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WslSeal {
    /// Whether this is WSL.
    wsl: bool,
    /// Whether the Windows drive is mounted here (`/mnt/c`).
    drives: bool,
    /// Whether Windows programs can be started from here (WSL interop).
    interop: bool,
}

impl WslSeal {
    fn here() -> Self {
        Self {
            wsl: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft")),
            drives: Path::new("/mnt/c/Windows").exists(),
            interop: std::fs::read_to_string("/proc/sys/fs/binfmt_misc/WSLInterop")
                .is_ok_and(|said| interop_enabled(&said)),
        }
    }
}

/// Whether WSL's interop entry, as `binfmt_misc` says it, is on.
fn interop_enabled(said: &str) -> bool {
    said.lines().next() == Some("enabled")
}

/// What doctor says of a WSL distro's seal: cut off from the Windows drive
/// and Windows programs, as Windows Setup leaves it, or not.
fn wsl_seal_said(seal: WslSeal) -> Finding {
    if !seal.wsl {
        return Finding::Unchecked(String::from("not WSL"));
    }
    let open: Vec<&str> = [
        (seal.drives, "it can read your Windows drive (/mnt/c)"),
        (
            seal.interop,
            "it can start Windows programs (WSL interop is on)",
        ),
    ]
    .into_iter()
    .filter(|(open, _)| *open)
    .map(|(_, what)| what)
    .collect();
    if open.is_empty() {
        Finding::Fine(String::from(
            "cut off from your Windows drives and Windows programs",
        ))
    } else {
        Finding::Note {
            what: format!(
                "this WSL distro isn't cut off from Windows: {}. Anything that escapes a cell here reaches them",
                open.join(", and ")
            ),
            fix: String::from(
                "In /etc/wsl.conf set [automount] enabled=false and [interop] enabled=false, then `wsl --terminate <distro>`. Windows Setup does this for its own distro",
            ),
        }
    }
}

/// What doctor says of Windows keeping Hatchling running (WO-054): WSL
/// stops the distro soon after its last window closes, and background work
/// with it, unless the Startup shortcut holds it up. Off Windows, the
/// service keeps the broker up.
fn keep_running_said(windows: bool, on: Option<bool>) -> Finding {
    match (windows, on) {
        (false, _) => Finding::Unchecked(String::from(
            "not a Windows install: the broker's service keeps it running",
        )),
        (true, Some(true)) => Finding::Fine(String::from(
            "on: Windows keeps Hatchling running while you're logged in, so background work wakes on time",
        )),
        (true, Some(false)) => Finding::Note {
            what: String::from(
                "off: Hatchling stops soon after its last window closes, so background work runs only while it's open",
            ),
            fix: String::from(KEEP_RUNNING_FIX),
        },
        (true, None) => Finding::Note {
            what: String::from("Windows hasn't said whether it keeps Hatchling running"),
            fix: String::from("Open Hatchling from the Start menu: it says, each time"),
        },
    }
}

/// How the owner turns keeping Hatchling running on.
const KEEP_RUNNING_FIX: &str =
    "Run Hatchling Setup again, and keep “Keep Hatchling running while I'm logged in” ticked";

/// What the owner can do about a vault anyone with the disk can open.
const WEAKER_VAULT_FIX: &str =
    "Protect it with a security key: it can't be opened without one (docs/security-keys.md)";

/// What doctor says of the agent's browser: off, in a cell of its own, or
/// set where it can't start, with cells held by process limits alone.
fn browser_said(set: bool, cgroup: bool) -> Finding {
    match (set, cgroup) {
        (false, _) => Finding::Fine(String::from(
            "off: the agent has no browser (HATCHLING_BROWSER isn't set)",
        )),
        (true, false) => Finding::problem(
            "HATCHLING_BROWSER is set, but cells are held by process limits alone: the agent's browser can't start without a cgroup",
            PROCESS_LIMITS_FIX,
        ),
        (true, true) => Finding::Fine(String::from(
            "in a cell of its own, reaching only the sites its session holds, through the broker's proxy",
        )),
    }
}

/// What doctor says of the agent's uid: one `nobody` shares with every
/// process running as nobody only where the operator accepted that.
fn nobody_said(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    match crate::api_auth::flag(
        lookup(env::HATCHLING_AGENT_ALLOW_NOBODY.name).as_deref(),
        "HATCHLING_AGENT_ALLOW_NOBODY",
    ) {
        Ok(false) => Finding::Fine(String::from("nobody isn't accepted as the agent's uid")),
        Ok(true) => Finding::Note {
            what: String::from(
                "HATCHLING_AGENT_ALLOW_NOBODY accepts nobody (65534) as the agent's uid: any process running as nobody could act as the agent",
            ),
            fix: String::from(
                "Give cells a uid of their own (HATCHLING_AGENT_UID), and unset HATCHLING_AGENT_ALLOW_NOBODY",
            ),
        },
        Err(_) => Finding::Unchecked(String::from("its setting doesn't parse (see settings)")),
    }
}

/// What a kernel without Landlock leaves the broker's own file access.
const NO_LANDLOCK: &str = "this kernel has no Landlock: the broker's own file access isn't confined, and a compromised broker reaches every file its user can";

/// What doctor says of the broker's own confinement, with the kernel's
/// Landlock `abi`, if it has one.
fn confinement_said(abi: Option<u32>) -> Finding {
    abi.map_or_else(
        || Finding::Note {
            what: String::from(NO_LANDLOCK),
            fix: String::from(
                "Use a kernel with Landlock on (Linux 5.13 or later; `cat /sys/kernel/security/lsm` lists landlock)",
            ),
        },
        |abi| Finding::Fine(format!("Landlock (ABI {abi}) confines the broker's own file access")),
    )
}

/// Whether a setting is set: given, and not blank.
fn is_set(value: Option<String>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

/// The system users a broker run as root gives its parts, each a user of
/// its own (WO-017), with the part each is for.
const SERVICE_USERS: [(&str, &str); 3] = [
    (
        crate::broker_user::BROKER_USER,
        "the broker's, once started",
    ),
    (crate::keeper::KEEPER_USER, "the vault keeper's"),
    (
        crate::egress_authority::EGRESS_USER,
        "the egress authority's",
    ),
];

/// A local account's uid and gid by its name: none when there's no such
/// account, or why it couldn't be looked up.
type Account<'a> = dyn Fn(&str) -> Result<Option<(u32, u32)>, String> + 'a;

/// What doctor says of who the Keeper and the Broker run as: a user of
/// their own each, as the `service` or run as root (`euid` 0), so each of
/// [`SERVICE_USERS`] must be an `account` that isn't root (WO-033);
/// otherwise the owner's own.
fn users_said(service: bool, euid: u32, account: &Account<'_>) -> Finding {
    if service || euid == 0 {
        let wrong: Vec<String> = SERVICE_USERS
            .iter()
            .filter_map(|(name, part)| match account(name) {
                Ok(Some((0, _))) => Some(format!("{name}, {part} user, is root")),
                Ok(Some(_)) => None,
                Ok(None) => Some(format!("there's no {name}, {part} user")),
                Err(why) => Some(format!("{name} couldn't be looked up: {why}")),
            })
            .collect();
        if !wrong.is_empty() {
            return Finding::Problem {
                what: format!("{}: the broker won't start without it", wrong.join("; ")),
                fix: String::from(
                    "Each is a system user of its own. `sudo systemd-sysusers` makes them from /etc/sysusers.d/hatchling.conf, which the installer put there; or run the README's `useradd` line for each",
                ),
            };
        }
        Finding::Fine(String::from(
            "the Keeper and the Broker each run as a user of their own",
        ))
    } else {
        Finding::Note {
            what: String::from(
                "run by hand, not as root: the Keeper and the Broker run as your own user, so neither is kept apart from you or from each other",
            ),
            fix: String::from(
                "Install Hatchling as a service (docs/install.md), or run it as root, which gives each a user of its own",
            ),
        }
    }
}

/// What doctor says of the output gate: whether the agent's replies to a
/// channel are checked for the owner's secrets and private keys.
fn output_gate_said(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    let Ok(config) = Config::parse(lookup) else {
        return Finding::Unchecked(String::from("the settings don't parse (see settings)"));
    };
    let Ok(gate) = crate::output_gate::RedactionConfig::parse(&config, lookup) else {
        return Finding::Unchecked(String::from("its settings don't parse (see settings)"));
    };
    if !gate.enabled {
        return Finding::Note {
            what: String::from(
                "the agent's replies to a channel aren't checked for your secrets before they go (HATCHLING_OUTPUT_GATE=off)",
            ),
            fix: String::from("Unset HATCHLING_OUTPUT_GATE, or set it to on"),
        };
    }
    let off: Vec<&str> = [
        (gate.builtins.secrets, "the secrets Hatchling keeps"),
        (gate.builtins.private_keys, "private keys"),
    ]
    .into_iter()
    .filter(|(on, _)| !on)
    .map(|(_, what)| what)
    .collect();
    if off.is_empty() {
        Finding::Fine(String::from(
            "the agent's replies to a channel are checked for your secrets and private keys before they go",
        ))
    } else {
        Finding::Note {
            what: format!(
                "the agent's replies to a channel aren't checked for {} (HATCHLING_REDACTION_BUILTINS)",
                off.join(" or ")
            ),
            fix: String::from("Unset HATCHLING_REDACTION_BUILTINS to check for both"),
        }
    }
}

/// What doctor says of the owner's decisions: kept where the scheduler is
/// on, and not otherwise.
fn decisions_said(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    if is_set(lookup(env::HATCHLING_SCHEDULER_DIR.name)) {
        Finding::Fine(String::from(
            "your decisions on the agent's requests are kept, and listed after a restart",
        ))
    } else {
        Finding::Note {
            what: String::from(
                "your decisions on the agent's requests aren't kept: the scheduler is off (HATCHLING_SCHEDULER_DIR isn't set), so the approvals history is empty after a restart",
            ),
            fix: String::from("Set HATCHLING_SCHEDULER_DIR (docs/install.md)"),
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
        return Finding::Note {
            what: String::from(
                "opens with its key file, by itself after a restart: anyone with this disk can open it",
            ),
            fix: String::from(WEAKER_VAULT_FIX),
        };
    };
    // Run as root, the keyring is the keeper's user's: read as theirs,
    // and nothing is made or given to anyone.
    if let (Ok(Some((uid, _))), Some(dir)) = (crate::keeper::keeper_user(), path.parent()) {
        let _ = hatchling_vault::act_for(uid, &[dir]);
    }
    if !path.exists() {
        return unmade_keyring(path);
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
        other @ (Finding::Fine(_)
        | Finding::Problem { .. }
        | Finding::Unchecked(_)
        | Finding::Note { .. }) => other,
    }
}

/// The stores' directories (WO-038): each as its reader takes it, or
/// named with the commands that put it right.
fn check_stores(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    check_stores_as(lookup, hatchling_sys::euid())
}

/// [`check_stores`], run by `euid`.
fn check_stores_as(lookup: &dyn Fn(&str) -> Option<String>, euid: u32) -> Finding {
    let Ok(config) = Config::parse(lookup) else {
        return Finding::Unchecked(String::from("the settings don't parse (see settings)"));
    };
    let keyring = crate::vault_keyring::KeyringConfig::parse(lookup)
        .ok()
        .and_then(|keyring| keyring.keyring);
    let dirs = stores::store_dirs(&config, keyring.as_deref(), lookup);
    stores::said(&stores::check(&dirs, stores::owner(euid)), euid == 0)
}

/// What doctor says where the settings name a keyring at `path` and none
/// is there: a setup that was cut short before it made one. The keeper
/// stops at every start until it's finished.
fn unmade_keyring(path: &Path) -> Finding {
    Finding::problem(
        format!(
            "the settings name a keyring at {}, and none is there: a setup stopped before it made one, so Hatchling can't start",
            path.display()
        ),
        "Run `sudo hatchling-broker vault-keyring setup`: it carries on from where it stopped.",
    )
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
        Tier::File => Finding::Note {
            what: String::from(
                "a keyring at the file tier: it opens by itself after a restart, and anyone with this disk can open it",
            ),
            fix: String::from(WEAKER_VAULT_FIX),
        },
        Tier::Totp => Finding::Note {
            what: String::from(
                "opens with a code from your authenticator app: after each restart, `sudo hatchling-broker unlock`. What makes the codes is on this disk, so anyone with this disk can open it",
            ),
            fix: String::from(WEAKER_VAULT_FIX),
        },
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

/// The room disk (0.9): whether this machine can have one, and how it is.
fn check_room_disk(lookup: &dyn Fn(&str) -> Option<String>) -> Finding {
    let Ok(disk) = crate::room_disk::RoomDisk::parse(lookup) else {
        return Finding::problem(
            "its settings don't parse",
            "Check HATCHLING_ROOM_DISK_DIR and HATCHLING_ROOM_DISK_SIZE (hatchling-broker --help).",
        );
    };
    let waiting = Config::parse(lookup)
        .is_ok_and(|config| crate::unlock_relay::socket_path(&config.vault).exists());
    room_said(&RoomFacts::here(&disk, waiting))
}

/// What [`check_room_disk`] looks at on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoomFacts {
    /// Whether doctor runs as root, where the room disk can be looked at.
    root: bool,
    /// cryptsetup's version, or why it can't be run.
    cryptsetup: Result<String, String>,
    /// What the kernel has of what the room disk needs.
    kernel: Kernel,
    /// How the room disk is, as root.
    status: Option<Result<crate::room_disk::RoomDiskStatus, String>>,
    /// The size a grow under way is growing the room disk to, as root.
    growing: Option<u64>,
    /// Whether the vault waits, locked, to be unlocked.
    waiting: bool,
}

impl RoomFacts {
    /// The facts here, of `disk`, with the vault `waiting` or not.
    fn here(disk: &crate::room_disk::RoomDisk, waiting: bool) -> Self {
        let root = hatchling_sys::euid() == 0;
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let modules = Path::new("/lib/modules").join(release.trim());
        let (status, growing) = kept_of(root.then(|| crate::room_disk::looked_at(disk)));
        Self {
            root,
            cryptsetup: crate::room_disk::cryptsetup_version(disk),
            kernel: Kernel {
                mapper: Path::new("/dev/mapper/control").exists(),
                loop_control: Path::new("/dev/loop-control").exists(),
                dm_crypt: kernel_has_dm_crypt(Path::new("/sys/module/dm_crypt"), &modules),
            },
            status,
            growing,
            waiting,
        }
    }
}

/// The room disk's status, as root, or why there's none, and a grow under
/// way: what doctor keeps of [`crate::room_disk::looked_at`]'s answer
/// (`looked`, none unless it runs as root).
fn kept_of(
    looked: Option<Result<(crate::room_disk::RoomDiskStatus, Option<u64>), String>>,
) -> (
    Option<Result<crate::room_disk::RoomDiskStatus, String>>,
    Option<u64>,
) {
    match looked {
        None => (None, None),
        Some(Ok((status, growing))) => (Some(Ok(status)), growing),
        Some(Err(why)) => (Some(Err(why)), None),
    }
}

/// What the kernel has of what the room disk needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kernel {
    /// Whether `/dev/mapper/control` is there: device-mapper.
    mapper: bool,
    /// Whether `/dev/loop-control` is there: loop devices.
    loop_control: bool,
    /// Whether the kernel has `dm_crypt`, loaded or to load.
    dm_crypt: bool,
}

/// Whether the kernel has `dm_crypt`: loaded (`loaded`, its `/sys/module`
/// entry), or a module or built in, as `modules` (its `/lib/modules/<release>`)
/// lists it.
fn kernel_has_dm_crypt(loaded: &Path, modules: &Path) -> bool {
    let listed = |file: &str| {
        std::fs::read_to_string(modules.join(file)).is_ok_and(|text| text.contains("dm-crypt.ko"))
    };
    loaded.exists() || listed("modules.dep") || listed("modules.builtin")
}

/// Bytes, as a person reads them.
fn bytes_said(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

/// What doctor says of the room disk, from `facts`. Off is a note, not a
/// problem: Hatchling works without the agent's computer. Open while the
/// vault is locked is a problem: a locked install mustn't hold it open. In
/// use by another process is fine, and not looked at: the agent's computer
/// has its disk while it runs. In use while the vault is locked is a
/// problem: it can't be seen to be closed.
fn room_said(facts: &RoomFacts) -> Finding {
    use crate::room_disk::RoomDiskState;
    let off = |what: String, fix: &str| Finding::Note {
        what: format!("the agent's computer is off here: {what}"),
        fix: fix.to_owned(),
    };
    let version = match &facts.cryptsetup {
        Ok(version) => version.clone(),
        Err(why) => {
            return off(
                format!("no cryptsetup to encrypt its disk ({why})"),
                "sudo apt-get install cryptsetup-bin, then sudo systemctl restart hatchling",
            );
        }
    };
    if !facts.kernel.mapper || !facts.kernel.dm_crypt {
        return off(
            String::from("this kernel has no device-mapper or no dm_crypt"),
            "a kernel with dm_crypt (Ubuntu's and WSL's have it)",
        );
    }
    if !facts.kernel.loop_control {
        return off(
            String::from("this kernel has no loop devices"),
            "a kernel with loop devices (Ubuntu's and WSL's have them)",
        );
    }
    let Some(status) = &facts.status else {
        return Finding::Unchecked(format!(
            "{version}; the room disk is root's: run doctor with sudo to see it"
        ));
    };
    let status = match status {
        Ok(status) => status,
        // The agent's computer has its disk for as long as it runs, and
        // maintenance has it while it works: the disk can't be looked at
        // from here, and nothing is wrong with it. With the vault locked
        // nothing should have it, and that it is closed can't be seen.
        Err(why) if why.starts_with(crate::room_disk::IN_USE) => {
            if facts.waiting {
                return Finding::problem(
                    "the room disk is in use while the vault is locked, so it can't be seen to be closed",
                    "End any hatchling-broker command still running, then run doctor again; if none is, sudo systemctl restart hatchling.",
                );
            }
            return Finding::Fine(format!(
                "{version}; in use now, by the agent's computer or by maintenance, so it isn't looked at"
            ));
        }
        Err(why) => {
            return Finding::problem(
                format!("the room disk can't be used: {why}"),
                "Check HATCHLING_ROOM_DISK_DIR names a directory of root's alone, apart from Hatchling's other state.",
            );
        }
    };
    if facts.waiting && matches!(status.state, RoomDiskState::Open | RoomDiskState::Mapped) {
        let how = if status.state == RoomDiskState::Open {
            "open"
        } else {
            "unlocked, though not mounted,"
        };
        return Finding::problem(
            format!(
                "the room disk is {how} while the vault is locked, so a disk taken now could read it"
            ),
            "sudo hatchling-broker room-disk close",
        );
    }
    room_numbers_said(&version, status, facts.growing)
}

/// What doctor says of a room disk that's fine to look at, `status`, with
/// cryptsetup's `version` and a grow under way (`growing`): its state, its
/// size, what it holds, what it takes below and what's free there.
fn room_numbers_said(
    version: &str,
    status: &crate::room_disk::RoomDiskStatus,
    growing: Option<u64>,
) -> Finding {
    use crate::room_disk::RoomDiskState;
    let state = match status.state {
        RoomDiskState::Absent => "none made yet",
        RoomDiskState::Closed => "made, closed",
        RoomDiskState::Mapped => "unlocked but not mounted: it couldn't be closed",
        RoomDiskState::Open => "open",
    };
    let size = if status.state == RoomDiskState::Absent {
        format!("it's made at {}", bytes_said(status.cap))
    } else {
        bytes_said(status.cap)
    };
    let holds = status
        .used
        .zip(status.free)
        .map_or_else(String::new, |(used, free)| {
            format!(
                ", holding {} with {} free",
                bytes_said(used),
                bytes_said(free)
            )
        });
    let takes = status.on_disk.map_or_else(String::new, |bytes| {
        format!(", taking {} of the disk below", bytes_said(bytes))
    });
    let said = format!("{version}; {state}; {size}{holds}{takes}");
    let on_disk = status.on_disk.unwrap_or(0);
    if let Some(free) = status
        .host_free
        .filter(|&free| free < status.cap.saturating_sub(on_disk))
    {
        return Finding::Note {
            what: format!(
                "{said}; only {} free below, less than it can grow to, and writes in it fail when that runs out",
                bytes_said(free)
            ),
            fix: String::from(
                "free some space, or set HATCHLING_ROOM_DISK_SIZE smaller before the room disk is made",
            ),
        };
    }
    let said = match status.host_free {
        Some(free) => format!("{said}; {} free below", bytes_said(free)),
        None => said,
    };
    room_grow_said(said, status, growing)
}

/// The end of what doctor says of a room disk that's there, `said` so far:
/// a grow under way (`growing`), or too little free in it, with how to
/// grow it.
fn room_grow_said(
    said: String,
    status: &crate::room_disk::RoomDiskStatus,
    growing: Option<u64>,
) -> Finding {
    use crate::room_disk::RoomDiskState;
    if let Some(to) = growing {
        // Open, the open's grow didn't finish: a finished one takes its
        // mark away.
        if status.state == RoomDiskState::Open {
            return Finding::Note {
                what: format!(
                    "{said}; its grow to {} didn't finish, so its file system is the size it was",
                    bytes_said(to)
                ),
                fix: String::from(
                    "sudo systemctl restart hatchling tries again; journalctl -u hatchling says why it didn't",
                ),
            };
        }
        return Finding::Fine(format!(
            "{said}; growing to {}, which finishes when Hatchling starts",
            bytes_said(to)
        ));
    }
    if status.free.is_some_and(|free| free < status.cap / 10) {
        return Finding::Note {
            what: format!(
                "{said}; less than a tenth of it is free, and the agent's writes in it fail when it's full"
            ),
            fix: String::from(
                "grow it (docs/storage.md): sudo systemctl stop hatchling, sudo hatchling-broker room-disk grow <size>, sudo systemctl start hatchling",
            ),
        };
    }
    Finding::Fine(said)
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
        Ok(()) => cells_fine(identity.mapping, identity.principal),
        Err((step, errno)) => {
            let (what, fix) = cell_advice(step, errno, Facts::read());
            Finding::problem(what, fix)
        }
    }
}

/// What doctor says of cells that work, mapped as `mapping`, run as
/// `principal`.
fn cells_fine(mapping: Mapping, principal: u32) -> Finding {
    if mapping == Mapping::Subordinate {
        Finding::Fine(String::from(
            "namespaces and the syscall filter work (the range's map helpers aren't tried here)",
        ))
    } else {
        Finding::Fine(format!(
            "a cell runs as uid {principal}, behind its syscall filter"
        ))
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
    /// No new privileges, then its syscall filter (WO-037).
    Filter,
}

impl Step {
    const ALL: [Self; 9] = [
        Self::User,
        Self::Maps,
        Self::Mount,
        Self::Net,
        Self::Ipc,
        Self::Root,
        Self::Pid,
        Self::Ended,
        Self::Filter,
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
            Self::Filter => 9,
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
    hatchling_sys::unshare_pid().map_err(|err| (Step::Pid, errno(&err)))?;
    // Last, as a cell does: no new privileges, then the agent's own
    // syscall filter, which a kernel without seccomp filtering refuses.
    hatchling_sys::set_no_new_privs().map_err(|err| (Step::Filter, errno(&err)))?;
    let profile = hatchling_sys::seccomp::SeccompProfile::agent()
        .map_err(|err| (Step::Filter, errno(&err)))?;
    hatchling_sys::seccomp::install(&profile).map_err(|err| (Step::Filter, errno(&err)))
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
            && step != Step::Filter
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
        Step::Filter => (
            format!(
                "a cell couldn't take its syscall filter{because}: without seccomp filtering, no cell starts"
            ),
            String::from(
                "Use a kernel with seccomp filtering on (CONFIG_SECCOMP_FILTER, Linux 3.5 or later): `grep Seccomp /proc/self/status` shows a Seccomp_filters line where it is.",
            ),
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
    let text = crate::windows_setup::here(&text);
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
            Finding::Note { what, fix } => {
                writeln!(
                    text,
                    "  note     {name:<14} {what}\n{:26}to change it: {fix}",
                    ""
                )
            }
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

    /// The stores' directories are checked as their readers take them:
    /// fine when right, and each wrong one named with its fix (WO-038).
    #[test]
    fn the_stores_are_checked_as_their_readers_take_them() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = crate::api_auth::tests::scratch("doctor-stores");
        let at = |name: &str| base.0.join(name);
        let settings: BTreeMap<String, String> = [
            ("HATCHLING_BROKER_SOCKET", at("run/broker.sock")),
            ("HATCHLING_AGENT_UID", PathBuf::from("2001")),
            ("HATCHLING_VAULT_DIR", at("vault")),
            ("HATCHLING_MEMORY_DIR", at("memory")),
            ("HATCHLING_GOALS_DIR", at("goals")),
        ]
        .into_iter()
        .map(|(key, path)| (String::from(key), path.display().to_string()))
        .collect();
        let lookup = |key: &str| settings.get(key).cloned();
        let me = hatchling_sys::euid();
        let (kind, words) = said(&check_stores_as(&lookup, me));
        assert_eq!(kind, "fine", "{words}");
        assert!(words.starts_with("none made yet"), "{words}");
        for dir in ["vault", "memory/user/985", "goals/goals/985"] {
            std::fs::create_dir_all(at(dir)).unwrap();
        }
        let mode = |dir: &str, mode: u32| {
            std::fs::set_permissions(at(dir), std::fs::Permissions::from_mode(mode)).unwrap();
        };
        for dir in [
            "vault",
            "memory",
            "memory/user",
            "memory/user/985",
            "goals",
            "goals/goals",
        ] {
            mode(dir, 0o700);
        }
        mode("goals/goals/985", 0o755);
        let (kind, words) = said(&check_stores_as(&lookup, me));
        assert_eq!(kind, "problem", "{words}");
        let wrong = at("goals/goals/985").display().to_string();
        assert!(words.contains(&format!("{wrong} (mode 755)")), "{words}");
        assert!(words.contains(&format!("`chmod 700 {wrong}`")), "{words}");
        mode("goals/goals/985", 0o700);
        assert_eq!(
            check_stores_as(&lookup, me),
            Finding::Fine(String::from(
                "7 directories checked: each its owner's alone, mode 700"
            ))
        );
        let broken = |key: &str| (key == "HATCHLING_VAULT_DIR").then(|| String::from("relative"));
        assert!(matches!(
            check_stores_as(&broken, me),
            Finding::Unchecked(_)
        ));
    }

    /// The vault's tiers a stolen disk opens are said so, with the
    /// stronger one: its key file, a keyring at the file tier, authenticator
    /// codes (WO-037).
    #[test]
    fn a_vault_a_stolen_disk_opens_is_said() {
        use hatchling_vault::Tier;
        let here = || Machine {
            keys: Vec::new(),
            wsl: false,
        };
        for tier in [Tier::File, Tier::Totp] {
            let (kind, words) = said(&vault_said(tier, 0, &here(), false));
            assert_eq!(kind, "note", "{words}");
            assert!(
                words.contains("anyone with this disk can open it"),
                "{words}"
            );
            assert!(words.contains("security key"), "{words}");
        }
        assert!(
            said(&vault_said(Tier::File, 0, &here(), false))
                .1
                .contains("by itself")
        );
        assert!(
            said(&vault_said(Tier::Totp, 0, &here(), false))
                .1
                .contains("authenticator app")
        );
        let none: BTreeMap<String, String> = BTreeMap::new();
        let (kind, words) = said(&check_vault(&|key: &str| none.get(key).cloned(), &here()));
        assert_eq!(kind, "note");
        assert!(
            words.contains("key file") && words.contains("anyone with this disk"),
            "{words}"
        );
    }

    /// The agent's computer, the Room: what stops it starting here is
    /// said, the first first; installed, it's fine (WO-037).
    #[test]
    fn a_room_that_cant_start_says_why() {
        use RoomMissing::{Admission, Arch, Cgroup, Image};
        let checks = |arch, admission, image, cgroup| {
            room_missing([
                (arch, Arch),
                (admission, Admission),
                (image, Image),
                (cgroup, Cgroup),
            ])
        };
        assert_eq!(checks(true, true, true, true), None);
        assert_eq!(checks(false, true, true, true), Some(Arch));
        assert_eq!(checks(true, false, true, true), Some(Admission));
        assert_eq!(checks(true, true, false, true), Some(Image));
        assert_eq!(checks(true, true, true, false), Some(Cgroup));
        assert_eq!(
            checks(false, false, true, true),
            Some(Arch),
            "the first first"
        );
        assert_eq!(said(&room_image_said(None)).0, "fine");
        // Each cause with the fix that answers it (#584): the installer and
        // Windows Setup only for a missing image, and no install advice
        // where an install wouldn't help.
        for (missing, why, fix) in [
            (
                Arch,
                "it needs x86_64 Linux",
                "| run Hatchling on an x86_64 machine",
            ),
            (
                Admission,
                "no image is installed",
                "| install a release that has its image, on x86_64: the installer adds it on Linux (docs/install.md), Windows Setup on Windows (docs/windows.md)",
            ),
            (
                Image,
                "its image isn't in place",
                "| install a release that has its image, on x86_64: the installer adds it on Linux",
            ),
            (Cgroup, "it needs a cgroup", "| give cells a cgroup"),
        ] {
            let (kind, words) = said(&room_image_said(Some(missing)));
            assert_eq!(kind, "note", "{words}");
            assert!(
                words.starts_with("the agent's computer (the Room) can't start here: ")
                    && words.contains(why)
                    && words.contains(fix),
                "{why}: {words}"
            );
            assert!(!words.contains("isn't packaged"), "{words}");
        }
    }

    /// A WSL distro not cut off from Windows is said, by what's open
    /// (WO-037).
    #[test]
    fn a_wsl_distro_open_to_windows_is_said() {
        let sealed = WslSeal {
            wsl: true,
            drives: false,
            interop: false,
        };
        assert_eq!(said(&wsl_seal_said(sealed)).0, "fine");
        assert_eq!(
            said(&wsl_seal_said(WslSeal {
                wsl: false,
                drives: true,
                interop: true
            })),
            ("unchecked", String::from("not WSL"))
        );
        let (kind, words) = said(&wsl_seal_said(WslSeal {
            drives: true,
            ..sealed
        }));
        assert_eq!(kind, "note");
        assert!(
            words.contains("read your Windows drive (/mnt/c)")
                && !words.contains("WSL interop is on"),
            "{words}"
        );
        let (kind, words) = said(&wsl_seal_said(WslSeal {
            interop: true,
            ..sealed
        }));
        assert_eq!(kind, "note");
        assert!(
            words.contains("WSL interop is on") && !words.contains("/mnt/c"),
            "{words}"
        );
        let (_, words) = said(&wsl_seal_said(WslSeal {
            drives: true,
            interop: true,
            ..sealed
        }));
        assert!(
            words.contains("(/mnt/c), and it can start Windows programs"),
            "{words}"
        );
    }

    /// Whether Windows keeps Hatchling running is said as Windows said it,
    /// and off Windows it isn't checked (WO-054).
    #[test]
    fn keeping_hatchling_running_is_said_as_windows_said_it() {
        for on in [None, Some(true), Some(false)] {
            assert_eq!(said(&keep_running_said(false, on)).0, "unchecked", "{on:?}");
        }
        let (kind, words) = said(&keep_running_said(true, Some(true)));
        assert_eq!(kind, "fine");
        assert!(
            words.starts_with("on: ") && words.contains("wakes on time"),
            "{words}"
        );
        let (kind, words) = said(&keep_running_said(true, Some(false)));
        assert_eq!(kind, "note");
        assert!(
            words.starts_with("off: ")
                && words.contains("only while it's open")
                && words.contains(KEEP_RUNNING_FIX),
            "{words}"
        );
        let (kind, words) = said(&keep_running_said(true, None));
        assert_eq!(kind, "note");
        assert!(
            words.contains("hasn't said") && words.contains("Open Hatchling"),
            "{words}"
        );
    }

    /// What a finding says, by its kind and words.
    fn said(finding: &Finding) -> (&'static str, String) {
        match finding {
            Finding::Fine(what) => ("fine", what.clone()),
            Finding::Note { what, fix } => ("note", format!("{what} | {fix}")),
            Finding::Problem { what, fix } => ("problem", format!("{what} | {fix}")),
            Finding::Unchecked(why) => ("unchecked", why.clone()),
        }
    }

    /// Without Landlock, doctor says the broker's own file access isn't
    /// confined (WO-037).
    #[test]
    fn a_broker_without_landlock_is_said() {
        let (kind, words) = said(&confinement_said(None));
        assert_eq!(kind, "note");
        assert!(words.starts_with(NO_LANDLOCK), "{words}");
        assert!(words.contains("a compromised broker reaches every file its user can"));
        assert_eq!(
            said(&confinement_said(Some(4))),
            (
                "fine",
                String::from("Landlock (ABI 4) confines the broker's own file access")
            )
        );
    }

    /// Run by hand, not as root, doctor says the Keeper and the Broker run
    /// as the owner (WO-037).
    #[test]
    fn a_hand_run_install_says_who_runs_it() {
        let all = |_: &str| -> Result<Option<(u32, u32)>, String> { Ok(Some((990, 990))) };
        let (kind, words) = said(&users_said(false, 1000, &all));
        assert_eq!(kind, "note");
        assert!(words.contains("run as your own user"), "{words}");
        // Run by hand, the users aren't needed, so a missing one isn't asked after.
        let asked = |_: &str| -> Result<Option<(u32, u32)>, String> { Err(String::from("asked")) };
        assert_eq!(said(&users_said(false, 1000, &asked)).0, "note");
        for (service, euid) in [(true, 1000), (false, 0), (true, 0)] {
            assert_eq!(
                said(&users_said(service, euid, &all)).0,
                "fine",
                "{service} {euid}"
            );
        }
        // A setting is set when it's given and not blank.
        assert!(is_set(Some(String::from("/usr/bin/x"))));
        assert!(!is_set(Some(String::from("  "))));
        assert!(!is_set(None));
    }

    /// As the service or root, a system user that's missing, root, or
    /// can't be looked up is a problem naming it, and how to make it
    /// (WO-033); each of the three is asked after.
    #[test]
    fn a_missing_system_user_is_named() {
        let asked = std::cell::RefCell::new(Vec::new());
        let without = |gone: &'static str, root: &'static str| {
            move |name: &str| -> Result<Option<(u32, u32)>, String> {
                Ok(if name == gone {
                    None
                } else if name == root {
                    Some((0, 0))
                } else {
                    Some((990, 990))
                })
            }
        };
        let recording = |name: &str| -> Result<Option<(u32, u32)>, String> {
            asked.borrow_mut().push(name.to_owned());
            Ok(Some((990, 990)))
        };
        assert_eq!(said(&users_said(true, 0, &recording)).0, "fine");
        assert_eq!(
            *asked.borrow(),
            ["hatchling-broker", "hatchling-keeper", "hatchling-egress"]
        );
        for (service, euid) in [(true, 1000), (false, 0)] {
            let (kind, words) = said(&users_said(service, euid, &without("hatchling-keeper", "")));
            assert_eq!(kind, "problem", "{service} {euid}");
            assert!(
                words.contains("there's no hatchling-keeper, the vault keeper's user")
                    && words.contains("systemd-sysusers")
                    && !words.contains("hatchling-egress"),
                "{words}"
            );
        }
        let (kind, words) = said(&users_said(true, 0, &without("", "hatchling-egress")));
        assert_eq!(kind, "problem");
        assert!(
            words.contains("hatchling-egress, the egress authority's user, is root"),
            "{words}"
        );
        let both = without("hatchling-broker", "hatchling-egress");
        let (_, words) = said(&users_said(true, 0, &both));
        assert!(
            words.contains("there's no hatchling-broker")
                && words.contains("hatchling-egress, the egress"),
            "each is named: {words}"
        );
        let unreadable = |_: &str| -> Result<Option<(u32, u32)>, String> {
            Err(String::from("cannot read /etc/passwd"))
        };
        let (kind, words) = said(&users_said(true, 0, &unreadable));
        assert_eq!(kind, "problem");
        assert!(
            words.contains("couldn't be looked up: cannot read /etc/passwd"),
            "{words}"
        );
    }

    /// The output gate off, or a built-in check off, is said (WO-037).
    #[test]
    fn an_output_gate_turned_off_is_said() {
        let settings = |extra: &[(&str, &str)]| {
            let mut all = BTreeMap::from([
                (
                    String::from("HATCHLING_BROKER_SOCKET"),
                    String::from("/run/hatchling-doctor-gate/broker.sock"),
                ),
                (String::from("HATCHLING_AGENT_UID"), String::from("2001")),
                (
                    String::from("HATCHLING_VAULT_DIR"),
                    String::from("/tmp/hatchling-doctor-gate/vault"),
                ),
            ]);
            for (key, value) in extra {
                all.insert((*key).to_owned(), (*value).to_owned());
            }
            all
        };
        let found = |extra: &[(&str, &str)]| {
            let all = settings(extra);
            said(&output_gate_said(&|key: &str| all.get(key).cloned()))
        };
        assert_eq!(found(&[]).0, "fine", "{:?}", found(&[]));
        let (kind, words) = found(&[("HATCHLING_OUTPUT_GATE", "off")]);
        assert_eq!(kind, "note");
        assert!(words.contains("aren't checked for your secrets"), "{words}");
        let (kind, words) = found(&[("HATCHLING_REDACTION_BUILTINS", "none")]);
        assert_eq!(kind, "note");
        assert!(
            words.contains("the secrets Hatchling keeps or private keys"),
            "{words}"
        );
        let (kind, words) = found(&[("HATCHLING_REDACTION_BUILTINS", "secret")]);
        assert_eq!(kind, "note");
        assert!(
            words.contains("aren't checked for private keys ("),
            "{words}"
        );
        assert_eq!(found(&[("HATCHLING_OUTPUT_GATE", "maybe")]).0, "unchecked");
    }

    /// A browser set where cells have no cgroup can't start, and doctor
    /// says so; process limits say the Room and the browser can't run
    /// (WO-037).
    #[test]
    fn a_browser_without_a_cgroup_is_said() {
        let (kind, words) = said(&browser_said(true, false));
        assert_eq!(kind, "problem");
        assert!(words.contains("can't start without a cgroup"), "{words}");
        assert_eq!(said(&browser_said(true, true)).0, "fine");
        assert!(said(&browser_said(false, false)).1.starts_with("off:"));
        assert!(
            PROCESS_LIMITS.contains("the agent's computer (the Room) and its browser can't run")
        );
    }

    /// Nobody accepted as the agent's uid is said (WO-037).
    #[test]
    fn nobody_as_the_agent_is_said() {
        let with = |value: Option<&str>| {
            let value = value.map(str::to_owned);
            said(&nobody_said(&|key: &str| {
                (key == "HATCHLING_AGENT_ALLOW_NOBODY")
                    .then(|| value.clone())
                    .flatten()
            }))
        };
        assert_eq!(with(None).0, "fine");
        assert_eq!(with(Some("false")).0, "fine");
        let (kind, words) = with(Some("true"));
        assert_eq!(kind, "note");
        assert!(
            words.contains("any process running as nobody could act as the agent"),
            "{words}"
        );
        assert_eq!(with(Some("perhaps")).0, "unchecked");
    }

    /// Decisions aren't kept where the scheduler is off, and doctor says
    /// so (WO-037).
    #[test]
    fn decisions_not_kept_are_said() {
        let none = |_: &str| None;
        let (kind, words) = said(&decisions_said(&none));
        assert_eq!(kind, "note");
        assert!(words.contains("aren't kept"), "{words}");
        let set = |key: &str| (key == "HATCHLING_SCHEDULER_DIR").then(|| String::from("/srv/s"));
        assert_eq!(said(&decisions_said(&set)).0, "fine");
        let blank = |key: &str| (key == "HATCHLING_SCHEDULER_DIR").then(|| String::from(" "));
        assert_eq!(said(&decisions_said(&blank)).0, "note");
    }

    /// How the vault opens, in each case: by itself, or after each restart
    /// with the owner; and, waiting now, what to do.
    /// The drop-in's setting is found among the unit's, and only that one.
    #[test]
    fn the_units_cell_cgroup_is_read_from_its_environment() {
        assert_eq!(
            cell_cgroup_in("A=1 HATCHLING_CELL_CGROUP=/sys/fs/cgroup/x B=2"),
            Some(String::from("/sys/fs/cgroup/x"))
        );
        assert_eq!(cell_cgroup_in("A=1 B=2"), None);
        assert_eq!(cell_cgroup_in(""), None);
    }

    /// A cgroup short of a controller is found before anyone unlocks.
    #[test]
    fn a_cgroup_without_its_controllers_is_a_problem() {
        use hatchling_sys::cgroup::DelegationProblem;
        // One that's there and lacks a controller: a reload gets it back.
        let (what, fix) = cell_limits_said("/c", &DelegationProblem::ParentLacks(vec!["cpu"]));
        assert_eq!(
            what,
            "the service's cgroup has no cpu controller, and cells won't run without one"
        );
        assert!(fix.contains("daemon-reload"), "{fix}");
        let found = cell_limits_found(true, Some((what.clone(), fix)));
        assert_eq!(problem(found), (what, fix.to_owned()));
        assert!(matches!(
            cell_limits_found(true, None),
            Finding::Fine(said) if said == "each cell has its own cgroup"
        ));
    }

    /// The installer's own doctor said cells each had a cgroup on a machine
    /// whose cgroups offered no controllers, and the broker then stopped at
    /// every start. A machine like that is a problem while the settings ask
    /// for a cgroup, and the fix is the installer's own.
    #[test]
    fn a_machine_with_no_controllers_is_a_problem_before_the_first_start() {
        use hatchling_sys::cgroup::DelegationProblem;
        let lacking = DelegationProblem::TreeLacks(vec!["pids", "memory", "cpu"]);
        let (what, fix) = cell_limits_said("/c", &lacking);
        assert!(
            what.starts_with("this machine's cgroups offer no pids, memory, cpu controller")
                && what.ends_with("the broker stops at every start"),
            "{what}"
        );
        assert!(
            fix.starts_with("Run the installer again")
                && fix.contains("hatchling.service.d/cgroup.conf"),
            "{fix}"
        );
        // A path that's no cgroup at all, by the filesystem itself.
        let (what, fix) = cell_limits_said("/c", &DelegationProblem::NotCgroup2);
        assert_eq!(
            what,
            "HATCHLING_CELL_CGROUP (/c) isn't in a cgroup v2 tree, and the broker stops at every start"
        );
        assert!(fix.contains("unset it"), "{fix}");
        let dir = scratch("not-a-cgroup");
        let (what, _) = problem(check_cell_limits(Some(&*dir.to_string_lossy())));
        assert!(what.contains("isn't in a cgroup v2 tree"), "{what}");
        // And one that isn't there yet is judged all the same: the
        // service's cgroup isn't, before the service's first start.
        let unmade = dir.join("not").join("made");
        let (what, _) = problem(check_cell_limits(Some(&*unmade.to_string_lossy())));
        assert!(what.contains("isn't in a cgroup v2 tree"), "{what}");
        // And a real one that offers every controller has nothing wrong.
        let offered =
            std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers").unwrap_or_default();
        if ["pids", "memory", "cpu"]
            .iter()
            .all(|needed| offered.split_whitespace().any(|name| name == *needed))
        {
            assert_eq!(cell_limits_problem(Some("/sys/fs/cgroup")), None);
        }
    }

    /// A cgroup that is there and short of a controller is put right by
    /// having systemd apply the unit's settings again, and said to have
    /// been; one that isn't put right, and any other problem, is left as
    /// it was found, with nothing run for the others.
    #[test]
    fn a_cgroup_short_of_a_controller_is_given_it_back() {
        use hatchling_sys::cgroup::DelegationProblem::{NotCgroup2, ParentLacks, TreeLacks};
        use std::cell::Cell;
        let lacks = || Some(ParentLacks(vec!["cpu"]));
        // Put right: nothing is left, and what came back is named.
        let (reapplied, looked) = (Cell::new(0), Cell::new(0));
        let reapply = |ok: bool| {
            let reapplied = &reapplied;
            move || {
                reapplied.set(reapplied.get() + 1);
                ok
            }
        };
        let again = |found: Option<hatchling_sys::cgroup::DelegationProblem>| {
            let looked = &looked;
            move || {
                looked.set(looked.get() + 1);
                found
            }
        };
        assert_eq!(
            restored(lacks(), reapply(true), again(None)),
            (None, vec!["cpu"])
        );
        assert_eq!((reapplied.get(), looked.get()), (1, 1));
        // systemd couldn't be asked: it is as it was, and isn't looked at.
        assert_eq!(
            restored(lacks(), reapply(false), again(None)),
            (lacks(), Vec::new())
        );
        assert_eq!((reapplied.get(), looked.get()), (2, 1));
        // Asked, and still short: what the second look found is left.
        let still = Some(ParentLacks(vec!["cpu", "memory"]));
        assert_eq!(
            restored(lacks(), reapply(true), again(still.clone())),
            (still, Vec::new())
        );
        assert_eq!((reapplied.get(), looked.get()), (3, 2));
        // Nothing wrong, or something a reload can't give: nothing is run.
        for other in [None, Some(NotCgroup2), Some(TreeLacks(vec!["cpu"]))] {
            assert_eq!(
                restored(other.clone(), reapply(true), again(None)),
                (other, Vec::new())
            );
        }
        assert_eq!((reapplied.get(), looked.get()), (3, 2));
    }

    /// After systemd is asked, the cgroup is looked at until nothing is
    /// wrong or the looks run out, with a pause between two looks and none
    /// after the last.
    #[test]
    fn the_cgroup_is_looked_at_until_it_is_right_or_the_looks_run_out() {
        use std::cell::Cell;
        let run = |clear_at: u32, looks: u32| {
            let (looked, paused) = (Cell::new(0_u32), Cell::new(0_u32));
            let found = settled(
                || {
                    looked.set(looked.get() + 1);
                    (looked.get() < clear_at).then_some("short")
                },
                || paused.set(paused.get() + 1),
                looks,
            );
            (found, looked.get(), paused.get())
        };
        assert_eq!(run(1, 5), (None, 1, 0), "right at the first look");
        assert_eq!(run(3, 5), (None, 3, 2), "right at the third");
        assert_eq!(run(9, 3), (Some("short"), 3, 2), "never right");
        assert_eq!(run(4, 3), (Some("short"), 3, 2), "right a look too late");
        assert_eq!(run(9, 1), (Some("short"), 1, 0), "one look");
        assert_eq!((REAPPLIED_LOOKS, REAPPLIED_PAUSE.as_millis()), (20, 100));
    }

    /// A program is said to have ended well only when it did.
    #[test]
    fn a_program_is_said_to_have_ended_well_only_when_it_did() {
        assert!(ran("true", "daemon-reload"));
        assert!(!ran("false", "daemon-reload"));
        assert!(!ran("/no/such/hatchling-program", "daemon-reload"));
    }

    /// What became of a cgroup is one of three things: nothing was wrong,
    /// it was given controllers back, or something is wrong still.
    #[test]
    fn what_became_of_a_cgroup_is_said_as_one_of_three_things() {
        use hatchling_sys::cgroup::DelegationProblem::ParentLacks;
        assert_eq!(became("/c", None, Vec::new()), CellLimits::Fine);
        assert_eq!(
            became("/c", None, vec!["cpu"]),
            CellLimits::Restored(vec!["cpu"])
        );
        let lacks = ParentLacks(vec!["cpu"]);
        let (what, fix) = cell_limits_said("/c", &lacks);
        assert_eq!(
            became("/c", Some(lacks), Vec::new()),
            CellLimits::Problem(what, fix)
        );
        assert_eq!(fix, CELL_LIMITS_FIX);
    }

    /// Looked at and not put right, a problem is said as it is found.
    #[test]
    fn a_cgroup_only_looked_at_is_said_as_it_is_found() {
        assert_eq!(cell_limits_seen(None), CellLimits::Fine);
        let (what, fix) = cell_limits_problem(Some("/proc/not-a-cgroup")).unwrap();
        assert_eq!(
            cell_limits_seen(Some("/proc/not-a-cgroup")),
            CellLimits::Problem(what, fix)
        );
    }

    /// With no cgroup set, or one that has nothing wrong, nothing is done;
    /// one that isn't a cgroup is a problem still, with nothing run.
    #[test]
    fn what_needs_no_putting_right_is_left_alone() {
        assert_eq!(cell_limits_restored(None), CellLimits::Fine);
        let CellLimits::Problem(what, fix) = cell_limits_restored(Some("/proc/not-a-cgroup"))
        else {
            return assert_eq!(
                cell_limits_restored(Some("/proc/not-a-cgroup")),
                CellLimits::Problem(String::new(), "")
            );
        };
        assert!(what.contains("isn't in a cgroup v2 tree"), "{what}");
        assert_eq!(fix, NOT_A_CGROUP_FIX);
        let offered =
            std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers").unwrap_or_default();
        if ["pids", "memory", "cpu"]
            .iter()
            .all(|needed| offered.split_whitespace().any(|name| name == *needed))
        {
            assert_eq!(
                cell_limits_restored(Some("/sys/fs/cgroup")),
                CellLimits::Fine
            );
        }
    }

    /// Cells held by process limits alone work, and are weaker: said as
    /// something to know, with how to change it, not as nothing.
    #[test]
    fn process_limits_alone_are_noted_as_weaker() {
        assert_eq!(cell_limits_problem(None), None);
        let Finding::Note { what, fix } = check_cell_limits(None) else {
            return assert_eq!(
                check_cell_limits(None),
                Finding::problem("", ""),
                "not a note"
            );
        };
        assert!(
            what.starts_with("cells are held by process limits alone")
                && what.contains("nothing bounds a cell's memory or CPU as a whole"),
            "{what}"
        );
        assert!(fix.contains("run the installer again"), "{fix}");
    }

    #[test]
    fn the_vault_says_how_it_opens_after_a_restart() {
        use hatchling_vault::Tier;
        let here = |keys: &[&str], wsl: bool| Machine {
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            wsl,
        };
        let fine = |finding: Finding| match finding {
            Finding::Fine(said) => said,
            other @ (Finding::Problem { .. } | Finding::Unchecked(_) | Finding::Note { .. }) => {
                format!("not fine: {other:?}")
            }
        };
        let fix = |finding: Finding| match finding {
            Finding::Problem { what, fix } => format!("{what} | {fix}"),
            other @ (Finding::Fine(_) | Finding::Unchecked(_) | Finding::Note { .. }) => {
                format!("not a problem: {other:?}")
            }
        };
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
        // A keyring that doesn't load: the problem.
        let dir = std::env::temp_dir().join(format!(
            "hatchling-doctor-broken-keyring-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keyring = dir.join("keyring");
        let named = BTreeMap::from([(
            String::from("HATCHLING_VAULT_KEYRING"),
            keyring.display().to_string(),
        )]);
        let found = || check_vault(&|key: &str| named.get(key).cloned(), &here(&[], false));
        // One the settings name that was never made: the setup finishes it.
        let (what, fix) = problem(found());
        assert!(
            what.contains("none is there") && what.contains(&*keyring.to_string_lossy()),
            "{what}"
        );
        assert!(fix.contains("vault-keyring setup"), "{fix}");
        // One that's there and isn't a keyring.
        std::fs::write(&keyring, b"not a keyring").unwrap();
        let (what, _) = problem(found());
        assert!(what.contains("doesn't load"), "{what}");
        std::fs::remove_dir_all(&dir).unwrap();
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
            other @ (Finding::Fine(_) | Finding::Unchecked(_) | Finding::Note { .. }) => {
                assert_eq!(other, Finding::problem("", ""), "not a problem");
                (String::new(), String::new())
            }
        }
    }

    /// The room disk's facts on a machine that has all it needs, with a
    /// room disk made and closed.
    fn room_facts() -> RoomFacts {
        RoomFacts {
            root: true,
            cryptsetup: Ok(String::from("cryptsetup 2.8.4")),
            kernel: Kernel {
                mapper: true,
                loop_control: true,
                dm_crypt: true,
            },
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                state: crate::room_disk::RoomDiskState::Closed,
                cap: 10 * 1024 * 1024 * 1024,
                image_bytes: Some(10 * 1024 * 1024 * 1024),
                on_disk: Some(300 * 1024 * 1024),
                used: None,
                free: None,
                host_free: Some(100 * 1024 * 1024 * 1024),
            })),
            growing: None,
            waiting: false,
        }
    }

    /// The room disk's facts, open, holding `used` of its 10 GiB with
    /// `free` free in it, all of it reserved below.
    fn room_open(used: u64, free: u64) -> RoomFacts {
        RoomFacts {
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                state: crate::room_disk::RoomDiskState::Open,
                on_disk: Some(10 * 1024 * 1024 * 1024 + 4096),
                used: Some(used),
                free: Some(free),
                ..room_facts().status.unwrap().unwrap()
            })),
            ..room_facts()
        }
    }

    /// The room disk's facts, with the room below it `host_free`.
    fn room_with_free(host_free: u64) -> RoomFacts {
        RoomFacts {
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                host_free: Some(host_free),
                ..room_facts().status.unwrap().unwrap()
            })),
            ..room_facts()
        }
    }

    /// What a note says, `what | fix`; anything else, that it isn't one.
    fn note_said(facts: &RoomFacts) -> String {
        match room_said(facts) {
            Finding::Note { what, fix } => format!("{what} | {fix}"),
            other @ (Finding::Fine(_) | Finding::Unchecked(_) | Finding::Problem { .. }) => {
                format!("not a note: {other:?}")
            }
        }
    }

    /// The room disk off, for want of cryptsetup, device-mapper, `dm_crypt`
    /// or loop devices, is a note and not a problem: an install doesn't
    /// stop at it, and a report with one says nothing's wrong. Open while
    /// the vault is locked is a problem, and so is a room disk that can't
    /// be used.
    #[test]
    fn the_room_disk_off_is_a_note_and_open_while_locked_a_problem() {
        use crate::room_disk::RoomDiskState;
        let without = RoomFacts {
            cryptsetup: Err(String::from("no such file")),
            ..room_facts()
        };
        let said = note_said(&without);
        assert!(said.contains("cryptsetup-bin"), "{said}");
        let kernel = room_facts().kernel;
        for missing in [
            Kernel {
                mapper: false,
                ..kernel
            },
            Kernel {
                dm_crypt: false,
                ..kernel
            },
        ] {
            let said = note_said(&RoomFacts {
                kernel: missing,
                ..room_facts()
            });
            assert!(said.contains("dm_crypt"), "{said}");
        }
        let said = note_said(&RoomFacts {
            kernel: Kernel {
                loop_control: false,
                ..kernel
            },
            ..room_facts()
        });
        assert!(said.contains("loop devices"), "{said}");
        let not_root = RoomFacts {
            root: false,
            status: None,
            ..room_facts()
        };
        assert!(matches!(room_said(&not_root), Finding::Unchecked(said) if said.contains("sudo")));
        let unusable = RoomFacts {
            status: Some(Err(String::from("can't be trusted"))),
            ..room_facts()
        };
        assert!(problem(room_said(&unusable)).0.contains("can't be trusted"));
        let open = |waiting| RoomFacts {
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                state: RoomDiskState::Open,
                ..room_facts().status.unwrap().unwrap()
            })),
            waiting,
            ..room_facts()
        };
        let (what, fix) = problem(room_said(&open(true)));
        assert!(what.contains("open while the vault is locked"), "{what}");
        assert_eq!(fix, "sudo hatchling-broker room-disk close");
        assert!(matches!(room_said(&open(false)), Finding::Fine(said) if said.contains("; open;")));
        let mapped = |waiting| RoomFacts {
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                state: RoomDiskState::Mapped,
                ..room_facts().status.unwrap().unwrap()
            })),
            waiting,
            ..room_facts()
        };
        let (what, _) = problem(room_said(&mapped(true)));
        assert!(
            what.contains("unlocked, though not mounted, while the vault is locked"),
            "{what}"
        );
        assert!(
            matches!(room_said(&mapped(false)), Finding::Fine(said) if said.contains("unlocked but not mounted"))
        );
        // A note isn't a problem: a report with one says nothing's wrong.
        let (text, problems) = rendered("here", &[("room disk", note_of(&without))]);
        assert_eq!(problems, 0, "a note was counted as a problem");
        assert!(text.contains("  note     room disk"), "{text}");
        assert!(text.contains("Nothing found wrong."), "{text}");
    }

    /// A room disk another process has is fine, and said so: the agent's
    /// computer has its disk while it runs, and a report with it says
    /// nothing's wrong. With the vault locked nothing should have the
    /// disk, and that is a problem. A refusal that only goes on to say so
    /// is a room disk that can't be used, as any other refusal is.
    #[test]
    fn a_room_disk_in_use_is_fine_unless_the_vault_is_locked() {
        let in_use = |waiting| RoomFacts {
            status: Some(Err(format!(
                "{}; stop Hatchling and retry maintenance",
                crate::room_disk::IN_USE
            ))),
            waiting,
            ..room_facts()
        };
        assert_eq!(
            room_said(&in_use(false)),
            Finding::Fine(String::from(
                "cryptsetup 2.8.4; in use now, by the agent's computer or by maintenance, so it isn't looked at"
            ))
        );
        let (text, problems) = rendered("here", &[("room disk", room_said(&in_use(false)))]);
        assert_eq!(problems, 0, "a room disk in use was counted as a problem");
        assert!(text.contains("  ok       room disk"), "{text}");
        assert!(text.contains("Nothing found wrong."), "{text}");
        let (what, fix) = problem(room_said(&in_use(true)));
        assert!(what.contains("in use while the vault is locked"), "{what}");
        assert!(fix.contains("sudo systemctl restart hatchling"), "{fix}");
        let other = RoomFacts {
            status: Some(Err(format!(
                "the room directory can't be trusted: {}",
                crate::room_disk::IN_USE
            ))),
            ..room_facts()
        };
        let (what, fix) = problem(room_said(&other));
        assert!(what.starts_with("the room disk can't be used: "), "{what}");
        assert!(fix.contains("HATCHLING_ROOM_DISK_DIR"), "{fix}");
    }

    /// A room disk that's fine is said in numbers: cryptsetup's version,
    /// its state, its cap, what it holds of the disk, and what's free
    /// below. Less room below than it can grow to is a note; room for all
    /// of it, to the byte, is enough.
    #[test]
    fn the_room_disk_is_said_in_numbers_and_too_little_room_is_a_note() {
        assert_eq!(
            room_said(&room_facts()),
            Finding::Fine(String::from(
                "cryptsetup 2.8.4; made, closed; 10.0 GiB, taking 300 MiB of the disk below; 100.0 GiB free below"
            ))
        );
        let none = RoomFacts {
            status: Some(Ok(crate::room_disk::RoomDiskStatus {
                state: crate::room_disk::RoomDiskState::Absent,
                image_bytes: None,
                on_disk: None,
                ..room_facts().status.unwrap().unwrap()
            })),
            ..room_facts()
        };
        assert_eq!(
            room_said(&none),
            Finding::Fine(String::from(
                "cryptsetup 2.8.4; none made yet; it's made at 10.0 GiB; 100.0 GiB free below"
            ))
        );
        let said = note_said(&room_with_free(1024 * 1024 * 1024));
        assert!(said.contains("less than it can grow to"), "{said}");
        assert!(
            matches!(
                room_said(&room_with_free(10 * 1024 * 1024 * 1024 - 300 * 1024 * 1024)),
                Finding::Fine(_)
            ),
            "room below for all the room disk can grow to was said to be too little"
        );
        assert_eq!(bytes_said(5 * 1024 * 1024), "5 MiB");
        assert_eq!(bytes_said(3 * 1024 * 1024 * 1024 / 2), "1.5 GiB");
    }

    /// Open, the room disk says what it holds and what's free in it; less
    /// than a tenth free is a note that says how to grow it, and a tenth,
    /// to the byte, is enough.
    #[test]
    fn an_open_room_disk_says_what_it_holds_and_nearly_full_says_to_grow_it() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            room_said(&room_open(3 * GIB / 2, 8 * GIB)),
            Finding::Fine(String::from(
                "cryptsetup 2.8.4; open; 10.0 GiB, holding 1.5 GiB with 8.0 GiB free, taking 10.0 GiB of the disk below; 100.0 GiB free below"
            ))
        );
        let said = note_said(&room_open(9 * GIB, GIB - 1));
        assert!(said.contains("less than a tenth of it is free"), "{said}");
        assert!(said.contains("room-disk grow <size>"), "{said}");
        assert!(
            matches!(room_said(&room_open(9 * GIB, GIB)), Finding::Fine(_)),
            "a tenth free was said to be too little"
        );
    }

    /// Doctor keeps the room disk's status and a grow under way from what
    /// it's told, as root; not as root, or with no status, there's no grow.
    #[test]
    fn doctor_keeps_the_status_and_a_grow_under_way() {
        let status = room_facts().status.unwrap().unwrap();
        assert_eq!(kept_of(None), (None, None));
        assert_eq!(
            kept_of(Some(Ok((status, Some(7))))),
            (Some(Ok(status)), Some(7))
        );
        assert_eq!(kept_of(Some(Ok((status, None)))), (Some(Ok(status)), None));
        assert_eq!(
            kept_of(Some(Err(String::from("in use")))),
            (Some(Err(String::from("in use"))), None)
        );
    }

    /// A grow under way is said: closed, it finishes when Hatchling starts;
    /// open, it didn't finish, and that's a note that says what to do.
    #[test]
    fn a_grow_under_way_is_said_and_one_left_unfinished_is_a_note() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let closed = RoomFacts {
            growing: Some(20 * GIB),
            ..room_facts()
        };
        assert_eq!(
            room_said(&closed),
            Finding::Fine(String::from(
                "cryptsetup 2.8.4; made, closed; 10.0 GiB, taking 300 MiB of the disk below; 100.0 GiB free below; growing to 20.0 GiB, which finishes when Hatchling starts"
            ))
        );
        let open = RoomFacts {
            growing: Some(20 * GIB),
            ..room_open(9 * GIB, GIB / 2)
        };
        let said = note_said(&open);
        assert!(
            said.contains("its grow to 20.0 GiB didn't finish"),
            "{said}"
        );
        assert!(said.contains("systemctl restart hatchling"), "{said}");
    }

    /// `dm_crypt` is loaded, or a module, or built in; or it isn't there.
    #[test]
    fn dm_crypt_is_seen_loaded_as_a_module_or_built_in() {
        let dir = crate::api_auth::tests::scratch("doctor-dm-crypt");
        let (loaded, modules) = (dir.0.join("dm_crypt"), dir.0.join("modules"));
        std::fs::create_dir(&modules).unwrap();
        assert!(!kernel_has_dm_crypt(&loaded, &modules));
        let list = |file: &str, text: &str| std::fs::write(modules.join(file), text).unwrap();
        list("modules.dep", "kernel/drivers/md/dm-mod.ko:\n");
        assert!(
            !kernel_has_dm_crypt(&loaded, &modules),
            "dm-mod taken for dm-crypt"
        );
        std::fs::create_dir(&loaded).unwrap();
        assert!(
            kernel_has_dm_crypt(&loaded, &modules),
            "a loaded dm_crypt wasn't seen"
        );
        std::fs::remove_dir(&loaded).unwrap();
        list(
            "modules.dep",
            "kernel/drivers/md/dm-crypt.ko: kernel/drivers/md/dm-mod.ko\n",
        );
        assert!(
            kernel_has_dm_crypt(&loaded, &modules),
            "a dm_crypt module wasn't seen"
        );
        std::fs::remove_file(modules.join("modules.dep")).unwrap();
        list("modules.builtin", "kernel/drivers/md/dm-crypt.ko\n");
        assert!(
            kernel_has_dm_crypt(&loaded, &modules),
            "a built-in dm_crypt wasn't seen"
        );
    }

    /// Doctor looks at the room disk here, as this test runs, and says what
    /// it finds; with settings that can't be read, it says so, and fails.
    #[test]
    fn doctor_looks_at_the_room_disk_here() {
        let here = RoomFacts::here(&crate::room_disk::RoomDisk::parse(|_| None).unwrap(), false);
        assert_eq!(here.root, hatchling_sys::euid() == 0);
        let _ = check_room_disk(&|_| None);
        assert_eq!(
            format!(
                "{:?}",
                doctor_main(&args(&["--env", "/nonexistent/hatchling.env"]))
            ),
            format!("{:?}", ExitCode::FAILURE)
        );
    }

    /// `facts`' finding, as it stands.
    fn note_of(facts: &RoomFacts) -> Finding {
        room_said(facts)
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
                assert_eq!(
                    seen,
                    &format!(
                        "a cell runs as uid {}, behind its syscall filter",
                        uid.unwrap()
                    )
                );
            }
            Finding::Problem { what, fix } => {
                assert!(!required, "cells are required: {what}");
                assert!(!what.is_empty() && !fix.is_empty());
            }
            Finding::Unchecked(why) => assert_eq!(why, "", "cells are always checked"),
            Finding::Note { what, .. } => assert_eq!(what, "", "cells are never a note"),
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

    /// Cells that work say how they're mapped, behind their filter; WSL's
    /// interop entry is read as `binfmt_misc` writes it (WO-037).
    #[test]
    fn working_cells_and_interop_are_read_as_said() {
        assert_eq!(
            said(&cells_fine(Mapping::Direct, 2001)),
            (
                "fine",
                String::from("a cell runs as uid 2001, behind its syscall filter")
            )
        );
        assert!(
            said(&cells_fine(Mapping::Subordinate, 2001))
                .1
                .starts_with("namespaces and the syscall filter work")
        );
        assert!(interop_enabled(
            "enabled
interpreter /init
"
        ));
        assert!(!interop_enabled(
            "disabled
interpreter /init
"
        ));
        assert!(!interop_enabled(""));
    }

    /// A filter refused is a kernel without seccomp filtering, never put
    /// down to `AppArmor` (WO-037).
    #[test]
    fn a_filter_refused_has_its_own_advice() {
        let facts = |root, restriction| Facts { root, restriction };
        let (said, fix) = cell_advice(Step::Filter, Some(22), facts(false, Restriction::None));
        assert!(said.contains("couldn't take its syscall filter"), "{said}");
        assert!(
            said.contains("without seccomp filtering, no cell starts"),
            "{said}"
        );
        assert!(fix.contains("CONFIG_SECCOMP_FILTER"), "{fix}");
        let (said, _) = cell_advice(Step::Filter, Some(1), facts(false, Restriction::AppArmor));
        assert!(said.contains("syscall filter"), "{said}");
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
