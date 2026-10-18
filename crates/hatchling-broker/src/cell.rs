//! Cell policy: what the broker's cells are, and the one call that
//! starts one.
//!
//! The broker owns every decision about a cell: its identity
//! (the configured uid base, which must equal the broker's
//! `HATCHLING_AGENT_UID`), which host directories it sees and whether it
//! may write them, its filter class, and its program, arguments and
//! environment. [`spawn_cell`] writes those decisions into a
//! [`CellSpec`] and hands it to the platform's isolation backend
//! (`hatchling-isolation`), which builds exactly that cell and
//! supervises it. Nothing here names a platform primitive.
//!
//! [`spawn_cell`] must be called before the broker starts its
//! connection threads: the backend forks, and forking a multi-threaded
//! process is unsound. In `main` the spawn happens between binding the
//! listener and `serve_forever`.
//!
//! The peer-credential story: `SO_PEERCRED` on the broker socket
//! reports the agent's uid as seen in the *broker's* namespace — the
//! shifted host uid, i.e. the id map's base. The operator therefore
//! sets `HATCHLING_AGENT_UID` to the cell's uid base
//! (`HATCHLING_CELL_UID_BASE`); the existing exact-equality peer gate
//! needs no change. In the other direction, the agent sees the broker
//! as the backend says it will (on Linux, the kernel overflow uid), and
//! the spawner hands that to the agent as `HATCHLING_BROKER_UID`.
//!
//! This module contains no `unsafe`, and every isolation mechanism goes
//! through the backend.

use std::path::{Path, PathBuf};

use hatchling_core::{LogLevel, SocketPath};
use hatchling_isolation::{
    Cell as _, CellIdentity, CellSpec, ENDPOINT_DIR, Endpoint, Exit, Exposure, Filter,
    IsolationBackend, Limits, Mapping, PROGRAM, Tool,
};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::environment as env;

/// Where a harness cell sees its work tree: its working
/// directory.
const IN_CELL_WORK: &str = "/work";

/// Where a harness cell sees its scratch directory: its `HOME`
/// and `TMPDIR`.
const IN_CELL_SCRATCH: &str = "/scratch";

/// Validated cell-spawner configuration.
///
/// Serializable so the broker can hand a prepared skill-cell config to
/// the spawner helper (see [`crate::skill_run`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellConfig {
    /// Host uid that in-cell uid 0 maps to. Always equals the broker's
    /// configured `HATCHLING_AGENT_UID`: [`CellConfig::from_env`]
    /// refuses to build a config where they differ, because
    /// `SO_PEERCRED` reports exactly this uid and the broker's peer
    /// gate would reject the agent.
    pub uid_base: u32,
    /// How many uids the map covers (in-cell `0..uid_count`).
    pub uid_count: u32,
    /// Host gid that in-cell gid 0 maps to, when it isn't `uid_base`'s
    /// number: a cell on the owner's subordinate ids.
    #[serde(default)]
    pub gid_base: Option<u32>,
    /// Where the cell's ids come from (`HATCHLING_CELL_IDS`).
    #[serde(default)]
    pub ids: CellIds,
    /// Absolute host path of the agent binary to run in the cell.
    pub agent_bin: PathBuf,
    /// Cell identity handed to the agent via `HATCHLING_CELL_ID`.
    pub cell_id: String,
    /// Absolute host path of the broker socket directory (bind-mounted
    /// at `/sock` in the cell).
    pub socket_dir: PathBuf,
    /// Absolute host path of the broker listen socket. [`spawn_cell`]
    /// gives the socket to [`CellConfig::uid_base`] before forking, and
    /// lets that uid traverse the directory, which stays the broker's, so
    /// the mapped agent can connect.
    pub socket_path: PathBuf,
    /// In-cell broker socket path handed to the agent
    /// (`/sock/<basename>`).
    pub in_cell_socket: String,
    /// The uid the agent sees for the broker: the broker's host uid
    /// translated through the cell's uid map (`uid - base` when mapped),
    /// or the kernel overflow uid when the broker is unmapped. Handed
    /// to the agent as `HATCHLING_BROKER_UID`; the agent's peer gate
    /// compares it against `SO_PEERCRED` with exact equality.
    pub broker_uid_in_cell: u32,
    /// Optional skill code directory for skill cells. When set, the
    /// directory is bind-mounted read-only at `/skill` in the cell, and
    /// the cell is a skill cell: no broker socket is mounted, and the
    /// program learns neither the socket's path nor the broker's uid.
    /// `None` for agent cells (no skill code). Set by
    /// [`crate::skill_cell::SkillCellConfig::new`], never by the agent
    /// path.
    pub skill_code: Option<PathBuf>,
    /// The scheduled run this cell executes (-2), if it is a run
    /// cell. The agent is started as `hatchling-agent run <task>`, and
    /// its environment gains exactly the run's own variables
    /// ([`ScheduledRun::env`]); nothing else about the cell changes.
    /// `None` for the broker's agent cell and for skill cells.
    #[serde(default)]
    pub scheduled: Option<ScheduledRun>,
    /// A harness cell's own part: its writable work tree and
    /// scratch directory, and the arguments the harness runs with. `None`
    /// for every other cell. A harness cell is a skill cell (no broker
    /// socket, nothing about the broker in its environment) that may
    /// write in exactly those two directories.
    #[serde(default)]
    pub harness: Option<HarnessDirs>,
    /// What the cell's program, and everything it starts, is held to.
    #[serde(default)]
    pub limits: CellLimits,
    /// Operator-delegated cgroup v2 parent. Unset uses the rlimit fallback.
    #[serde(default)]
    pub cgroup: Option<PathBuf>,
    /// Whether the agent reaches the broker over a connection made for
    /// this cell alone ([`CellHandle::connection`]) rather than the
    /// broker's socket. A connected cell sees no socket, and learns
    /// nothing about one. Set for the broker's own agent cell.
    #[serde(default)]
    pub connected: bool,
}

/// Where cells get their uid and gid: `HATCHLING_CELL_IDS`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellIds {
    /// The broker's own map (`own`, the default): any uid for a root
    /// broker, `HATCHLING_CELL_UID_BASE` or nobody's; an unprivileged
    /// broker's own uid, which its cells then share.
    #[default]
    Own,
    /// The owner's subordinate ids (`subordinate`, WO-014): the first of
    /// its `/etc/subuid` and `/etc/subgid` ranges, mapped by
    /// `newuidmap` and `newgidmap`, so an unprivileged broker's cells
    /// get a uid of their own.
    Subordinate,
}

impl CellIds {
    /// Reads `HATCHLING_CELL_IDS` through `get`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] for anything but `own` or `subordinate`.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        match get(env::HATCHLING_CELL_IDS.name).as_deref() {
            None | Some("own") => Ok(Self::Own),
            Some("subordinate") => Ok(Self::Subordinate),
            Some(_) => Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_IDS",
                reason: String::from("expected own or subordinate"),
            }),
        }
    }
}

/// How to give cells a uid other than the broker's: the end of every
/// refusal to serve a socket the agent's uid could reach.
pub(crate) const OWN_UID_HINT: &str = "give cells a uid of their own: run the broker as root, \
     or set HATCHLING_CELL_IDS=subordinate, with a range for this user in /etc/subuid and \
     /etc/subgid and HATCHLING_AGENT_UID set to the first id of its /etc/subuid range";

/// The identity a cell gets from `backend`: the owner's subordinate ids
/// when `ids` says so, otherwise `count` uids from `principal` (or the
/// backend's default).
///
/// # Errors
///
/// [`Error::InvalidConfig`] if a subordinate cell is also given a base
/// or a count (its ranges decide them), or the backend's refusal.
fn cell_identity(
    backend: &impl IsolationBackend,
    ids: CellIds,
    principal: Option<u32>,
    count: Option<u32>,
) -> Result<CellIdentity, Error> {
    match ids {
        CellIds::Own => Ok(backend.identity(principal, count.unwrap_or(1))?),
        CellIds::Subordinate if principal.is_some() || count.is_some() => {
            Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_IDS",
                reason: String::from(
                    "subordinate cells take their uid and gid from /etc/subuid and /etc/subgid: \
                     leave HATCHLING_CELL_UID_BASE and HATCHLING_CELL_UID_COUNT unset",
                ),
            })
        }
        CellIds::Subordinate => Ok(backend.subordinate_identity()?),
    }
}

/// What every cell's program, and everything it starts, is held to.
///
/// The limits come from `HATCHLING_CELL_MAX_PROCESSES`, `HATCHLING_CELL_MAX_ADDRESS_SPACE`,
/// `HATCHLING_CELL_MAX_FILE_SIZE` and `HATCHLING_CELL_MAX_OPEN_FILES`:
/// each a positive count or number of bytes, or `unlimited` to leave the
/// broker's own. Each is both the cell's soft and its hard limit, so the
/// cell can't lift it; a value above the broker's own hard limit is held
/// at the broker's. With `HATCHLING_CELL_CGROUP`, processes count the whole
/// cell, and `HATCHLING_CELL_MAX_MEMORY` and `HATCHLING_CELL_MAX_CPUS` set its
/// aggregate memory and CPU limits. Without a group those two have no effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellLimits {
    /// How many processes, threads included, a cell may hold at once:
    /// 4096 unless configured.
    pub processes: Option<u64>,
    /// A program's address space, in bytes: unlimited unless configured.
    /// It isn't a memory bound: a runtime such as V8 reserves far more
    /// address space than it uses.
    pub address_space: Option<u64>,
    /// The largest file a program may write, in bytes: 8 GiB unless
    /// configured.
    pub file_size: Option<u64>,
    /// How many files a program may hold open at once: 8192 unless
    /// configured.
    pub open_files: Option<u64>,
    /// Whole-cell memory in bytes; only effective with a cgroup.
    #[serde(default)]
    pub memory: Option<u64>,
    /// CPU allowance in thousandths of a core; only effective with a cgroup.
    #[serde(default)]
    pub cpu_millis: Option<u64>,
}

impl Default for CellLimits {
    fn default() -> Self {
        Self {
            processes: Some(4096),
            address_space: None,
            file_size: Some(8 << 30),
            open_files: Some(8192),
            memory: None,
            cpu_millis: None,
        }
    }
}

impl CellLimits {
    /// The limits `get` sets; an unset one is its default.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a value that is neither a
    /// positive base-10 number nor `unlimited`.
    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let defaults = Self::default();
        Ok(Self {
            processes: limit(
                &get,
                env::HATCHLING_CELL_MAX_PROCESSES.name,
                defaults.processes,
            )?,
            address_space: limit(
                &get,
                env::HATCHLING_CELL_MAX_ADDRESS_SPACE.name,
                defaults.address_space,
            )?,
            file_size: limit(
                &get,
                env::HATCHLING_CELL_MAX_FILE_SIZE.name,
                defaults.file_size,
            )?,
            open_files: limit(
                &get,
                env::HATCHLING_CELL_MAX_OPEN_FILES.name,
                defaults.open_files,
            )?,
            memory: limit(
                &get,
                env::HATCHLING_CELL_MAX_MEMORY.name,
                if get(env::HATCHLING_CELL_CGROUP.name).is_some() {
                    Some(hatchling_sys::cgroup::default_memory().map_err(|source| {
                        Error::InvalidConfig {
                            key: "HATCHLING_CELL_MAX_MEMORY",
                            reason: source.to_string(),
                        }
                    })?)
                } else {
                    None
                },
            )?,
            cpu_millis: cpu_limit(get(env::HATCHLING_CELL_MAX_CPUS.name).as_deref())?,
        })
    }
}

impl From<CellLimits> for Limits {
    fn from(limits: CellLimits) -> Self {
        Self {
            processes: limits.processes,
            address_space: limits.address_space,
            file_size: limits.file_size,
            open_files: limits.open_files,
            memory: limits.memory,
            cpu_millis: limits.cpu_millis,
        }
    }
}

/// One limit: `key`'s value, a positive base-10 number or `unlimited`
/// for none, or `default` when it's unset.
fn limit(
    get: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: Option<u64>,
) -> Result<Option<u64>, Error> {
    match get(key).as_deref() {
        None => Ok(default),
        Some("unlimited") => Ok(None),
        Some(raw) => match raw.parse::<u64>() {
            Ok(0) | Err(_) => Err(Error::InvalidConfig {
                key,
                reason: String::from("expected a positive base-10 number, or `unlimited`"),
            }),
            Ok(value) => Ok(Some(value)),
        },
    }
}

/// Keeps malformed settings present, so validation cannot select a default.
/// Every cell setting read here rejects an empty value.
pub(crate) fn env_value(raw: Result<String, std::env::VarError>) -> Option<String> {
    match raw {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => Some(String::new()),
    }
}

/// Validates an operator-provided cgroup parent; unset uses rlimits alone.
pub(crate) fn cgroup_parent(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<PathBuf>, Error> {
    get(env::HATCHLING_CELL_CGROUP.name)
        .map(|raw| {
            let parent = PathBuf::from(raw);
            hatchling_sys::cgroup::prepare_parent(&parent).map_err(|source| {
                Error::InvalidConfig {
                    key: "HATCHLING_CELL_CGROUP",
                    reason: source.to_string(),
                }
            })?;
            Ok(parent)
        })
        .transpose()
}

/// A fractional core count, represented exactly in thousandths of a core.
fn cpu_limit(raw: Option<&str>) -> Result<Option<u64>, Error> {
    let refused = || Error::InvalidConfig {
        key: "HATCHLING_CELL_MAX_CPUS",
        reason: String::from(
            "expected a positive core count (at least 0.01, up to three decimal places), or `unlimited`",
        ),
    };
    let raw = match raw {
        None | Some("unlimited") => return Ok(None),
        Some(raw) => raw,
    };
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(refused());
    }
    let whole = whole.parse::<u64>().map_err(|_| refused())?;
    let fractional = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().map_err(|_| refused())? * 100,
        2 => fraction.parse::<u64>().map_err(|_| refused())? * 10,
        3 => fraction.parse::<u64>().map_err(|_| refused())?,
        _ => return Err(refused()),
    };
    whole
        .checked_mul(1000)
        .and_then(|millis| millis.checked_add(fractional))
        .filter(|millis| *millis >= 10 && millis.checked_mul(100).is_some())
        .map(Some)
        .ok_or_else(refused)
}

/// A harness cell's writable directories and arguments.
///
/// In the cell the work tree is `/work` (the harness's working
/// directory) and the scratch directory `/scratch` (its home and temp).
/// Both are bound writable, `nosuid,nodev`; everything else stays
/// read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDirs {
    /// The host directory bound at `/work`.
    pub work: PathBuf,
    /// The host directory bound at `/scratch`.
    pub scratch: PathBuf,
    /// The harness's arguments, after its program path.
    pub args: Vec<String>,
    /// A real CLI's network: `None` for a harness that runs
    /// offline, under the work-tree filter.
    pub network: Option<HarnessNetwork>,
}

/// What a real CLI's cell adds: its relay, the tools it may run,
/// and the environment that points it at the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessNetwork {
    /// The relay's port on the cell's own loopback.
    pub relay: u16,
    /// The unix socket, in the broker's private run directory, where the
    /// spawner hands the cell's relay listener to the broker.
    pub handoff: PathBuf,
    /// Host programs the cell may run, and the name it sees each at.
    pub tools: Vec<(PathBuf, String)>,
    /// The CLI's own environment: its relay URL, its surrogate key, its
    /// `PATH`, and its quiet settings.
    pub env: Vec<(String, String)>,
}

/// One scheduled run, as its cell receives it (-2).
///
/// Typed rather than free-form: a run cell can receive only these
/// values, under these names. The `missed` flag and the due time are
/// the broker's assertion, not the agent's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledRun {
    /// The task text, passed as the `run` subcommand's argument.
    pub task: String,
    /// Whether this fire is late because the broker missed it.
    pub missed: bool,
    /// When the fire was due, in Unix seconds.
    pub due: i64,
    /// The model endpoint the run uses, if the operator configured one.
    pub model_endpoint: Option<String>,
    /// The model name, likewise.
    pub model_name: Option<String>,
    /// Thinking mode for the ordinary model client; older specs retain the default.
    #[serde(default)]
    pub model_reasoning: hatchling_core::ModelReasoning,
    /// The run's depth in its follow-up chain: 0 unless it is a
    /// follow-up.
    #[serde(default)]
    pub depth: u32,
    /// For a hook run, the event that fired it: the broker's
    /// own value, never text the agent wrote.
    #[serde(default)]
    pub event: Option<hatchling_core::HookSource>,
    /// For a subagent's run, its step budget: the agent's own
    /// loop bound, set by the broker.
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Whether the run is a subagent: its agent then reports how
    /// it ended in a typed envelope.
    #[serde(default)]
    pub subagent: bool,
    /// Whether the run answers the user's message on the local channel:
    /// its agent then speaks with the chat prompt.
    #[serde(default)]
    pub chat: bool,
}

impl ScheduledRun {
    /// The variables a run cell adds to the agent environment, in a
    /// fixed order.
    #[must_use]
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![
            (
                "HATCHLING_RUN_MISSED",
                String::from(if self.missed { "1" } else { "0" }),
            ),
            ("HATCHLING_RUN_DUE", self.due.to_string()),
            ("HATCHLING_RUN_DEPTH", self.depth.to_string()),
        ];
        if let Some(event) = &self.event {
            env.push(("HATCHLING_RUN_EVENT", event.to_string()));
        }
        if let Some(steps) = self.max_steps {
            env.push(("HATCHLING_MODEL_MAX_STEPS", steps.to_string()));
        }
        if self.subagent {
            env.push(("HATCHLING_RUN_SUBAGENT", String::from("1")));
        }
        if self.chat {
            env.push(("HATCHLING_RUN_CHAT", String::from("1")));
        }
        if let Some(endpoint) = &self.model_endpoint {
            env.push(("HATCHLING_MODEL_ENDPOINT", endpoint.clone()));
        }
        if let Some(name) = &self.model_name {
            env.push(("HATCHLING_MODEL_NAME", name.clone()));
        }
        if self.model_reasoning != hatchling_core::ModelReasoning::ProviderDefault {
            env.push((
                "HATCHLING_MODEL_REASONING",
                self.model_reasoning.as_setting().to_owned(),
            ));
        }
        env
    }

    /// The agent's argv: `run` and the task.
    #[must_use]
    pub fn argv(&self) -> [&str; 3] {
        [PROGRAM, "run", self.task.as_str()]
    }
}

impl CellConfig {
    /// Holds cleanup ownership in the broker while a helper builds its cell.
    /// The helper gets a private parent; killing it cannot strand its leaves.
    pub(crate) fn supervised_cgroup(
        &self,
    ) -> Result<(Self, Option<hatchling_sys::cgroup::CellCgroup>), Error> {
        let group = self
            .cgroup
            .as_ref()
            .map(|parent| {
                hatchling_sys::cgroup::CellCgroup::create(
                    parent,
                    hatchling_sys::cgroup::CgroupLimits::default(),
                )
            })
            .transpose()
            .map_err(|source| Error::InvalidConfig {
                key: "HATCHLING_CELL_CGROUP",
                reason: source.to_string(),
            })?;
        let mut cell = self.clone();
        if let Some(group) = &group {
            cell.cgroup = Some(group.path().to_path_buf());
        }
        Ok((cell, group))
    }
    /// Refuses a vault the agent cell could see: the cell
    /// bind-mounts the broker socket's directory at `/sock`, so neither the
    /// vault root nor its key file may lie inside it. With an unprivileged
    /// broker the cell's uid owns the vault, so a vault there would be
    /// readable from inside.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] naming the setting that put the
    /// vault or its key in the cell's view.
    pub fn refuse_vault_in_view(&self, vault: &crate::VaultConfig) -> Result<(), Error> {
        for (key, path) in [
            ("HATCHLING_VAULT_DIR", &vault.root),
            ("HATCHLING_VAULT_KEY_FILE", &vault.key_file),
        ] {
            if path.starts_with(&self.socket_dir) {
                return Err(Error::InvalidConfig {
                    key,
                    reason: format!(
                        "{} is inside the broker socket directory {}, which the agent cell mounts",
                        path.display(),
                        self.socket_dir.display()
                    ),
                });
            }
        }
        Ok(())
    }

    /// The base config for a skill cell: the default uid map (as
    /// [`CellConfig::from_env`] would choose it), the limits the
    /// environment sets ([`CellLimits`]), no program yet, and no
    /// broker socket. [`prepare_skill_cell`](crate::skill_cell::prepare_skill_cell)
    /// sets the verified program and code dir, which makes it a skill
    /// cell.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CellMap`] if the default map fails policy
    /// validation for this broker's privilege level, or
    /// [`Error::InvalidConfig`] for a limit that doesn't parse.
    pub fn skill(cell_id: &str) -> Result<Self, Error> {
        Self::skill_with(cell_id, |key| env_value(std::env::var(key)))
    }

    /// [`CellConfig::skill`], reading its configuration through `get`.
    fn skill_with(cell_id: &str, get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let backend = hatchling_isolation::platform();
        let ids = CellIds::parse(&get)?;
        let identity = cell_identity(&backend, ids, None, None)?;
        let broker_uid_in_cell = backend.broker_in_cell(&identity)?;
        Ok(Self {
            uid_base: identity.principal,
            uid_count: identity.count,
            gid_base: (ids == CellIds::Subordinate).then_some(identity.group),
            ids,
            agent_bin: PathBuf::new(),
            cell_id: cell_id.to_owned(),
            socket_dir: PathBuf::new(),
            socket_path: PathBuf::new(),
            in_cell_socket: String::new(),
            broker_uid_in_cell,
            skill_code: None,
            scheduled: None,
            harness: None,
            limits: CellLimits::parse(&get)?,
            cgroup: cgroup_parent(&get)?,
            connected: false,
        })
    }

    /// The identity the cell runs as.
    #[must_use]
    pub const fn identity(&self) -> CellIdentity {
        CellIdentity {
            principal: self.uid_base,
            group: match self.gid_base {
                Some(gid) => gid,
                None => self.uid_base,
            },
            count: self.uid_count,
            mapping: match self.ids {
                CellIds::Own => Mapping::Direct,
                CellIds::Subordinate => Mapping::Subordinate,
            },
        }
    }

    /// A run cell for one scheduled run (-2): the agent binary at
    /// `agent_bin`, reaching the broker only through the run's own
    /// socket at `socket` (whose directory is the only host path it
    /// mounts besides the binary and its libraries), with the uid map
    /// [`CellConfig::from_env`] would choose for `agent_uid`.
    ///
    /// # Errors
    ///
    /// As [`CellConfig::from_env`], and [`Error::InvalidConfig`] for a
    /// task containing a NUL byte (it could not be passed to `exec`).
    pub fn for_run(
        agent_bin: &Path,
        cell_id: &str,
        socket: &SocketPath,
        agent_uid: u32,
        run: ScheduledRun,
    ) -> Result<Self, Error> {
        if run.task.contains('\0') {
            return Err(Error::InvalidConfig {
                key: "scheduled run",
                reason: String::from("the task contains a NUL byte"),
            });
        }
        let agent_bin = agent_bin.to_string_lossy().into_owned();
        let mut config = Self::parse(
            |key| match key {
                "HATCHLING_AGENT_BIN" => Some(agent_bin.clone()),
                "HATCHLING_CELL_ID" => Some(cell_id.to_owned()),
                other => env_value(std::env::var(other)),
            },
            socket,
            agent_uid,
        )?;
        config.scheduled = Some(run);
        Ok(config)
    }

    /// Whether this is a skill cell (see [`CellConfig::skill_code`]).
    #[must_use]
    pub const fn is_skill_cell(&self) -> bool {
        self.skill_code.is_some()
    }

    /// Whether this is a harness cell: a skill cell with a
    /// writable work tree and scratch directory.
    #[must_use]
    pub const fn is_harness_cell(&self) -> bool {
        self.harness.is_some()
    }

    /// Reads and validates cell configuration from the environment.
    ///
    /// `HATCHLING_AGENT_BIN` and `HATCHLING_CELL_ID` are required.
    /// `HATCHLING_CELL_UID_BASE` defaults to the nobody uid (65534) for
    /// a root broker and to the broker's own uid otherwise — the only
    /// map an unprivileged broker can install. `HATCHLING_CELL_UID_COUNT`
    /// defaults to 1. The `HATCHLING_CELL_MAX_*` limits are
    /// [`CellLimits`]'s.
    ///
    /// `agent_uid` is the broker's configured `HATCHLING_AGENT_UID`
    /// (what its peer gate expects `SO_PEERCRED` to report). It must
    /// equal the id-map base: a cell whose agent the broker would
    /// reject is never spawned.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if required values are missing, paths are not
    /// absolute, the id map fails policy validation, or `agent_uid`
    /// differs from the map base.
    pub fn from_env(socket: &SocketPath, agent_uid: u32) -> Result<Self, Error> {
        Self::parse(|key| env_value(std::env::var(key)), socket, agent_uid)
    }

    /// Builds a cell config for tests without touching process
    /// environment: the agent binary, cell id, and socket directory
    /// are given directly; the uid base defaults exactly as
    /// [`CellConfig::from_env`] would, and must equal `agent_uid`.
    ///
    /// This is `pub` so integration tests can construct the exact
    /// config `spawn_cell` needs. Production code uses `from_env`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the constructed config fails validation
    /// (e.g. `agent_uid` differs from the default uid base).
    pub fn parse_test(
        agent_bin: &Path,
        cell_id: &str,
        socket_dir: &Path,
        agent_uid: u32,
    ) -> Result<Self, Error> {
        let get = |key: &str| match key {
            "HATCHLING_AGENT_BIN" => Some(agent_bin.to_string_lossy().into_owned()),
            "HATCHLING_CELL_ID" => Some(cell_id.to_owned()),
            _ => None,
        };
        let socket_path = socket_dir.join("broker.sock");
        let socket: SocketPath = socket_path
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::InvalidConfig {
                key: "test socket path",
                reason: String::from("not valid UTF-8 or does not parse"),
            })?;
        Self::parse(get, &socket, agent_uid)
    }

    /// Variant of [`CellConfig::parse_test`] that returns the parse
    /// error instead of panicking: used to assert fail-closed config
    /// mismatches without forking.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the config parses successfully (the test
    /// expects failure), or the parse error itself on mismatch.
    pub fn parse_test_err(
        agent_bin: &Path,
        cell_id: &str,
        socket_dir: &Path,
        uid_base: u32,
        agent_uid: u32,
    ) -> Result<Error, Error> {
        let get = |key: &str| match key {
            "HATCHLING_AGENT_BIN" => Some(agent_bin.to_string_lossy().into_owned()),
            "HATCHLING_CELL_ID" => Some(cell_id.to_owned()),
            "HATCHLING_CELL_UID_BASE" => Some(uid_base.to_string()),
            _ => None,
        };
        let socket_path = socket_dir.join("broker.sock");
        let socket: SocketPath = socket_path
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::InvalidConfig {
                key: "test socket path",
                reason: String::from("not valid UTF-8 or does not parse"),
            })?;
        match Self::parse(get, &socket, agent_uid) {
            Ok(_) => Err(Error::InvalidConfig {
                key: "test",
                reason: String::from("expected parse to fail"),
            }),
            Err(e) => Ok(e),
        }
    }

    /// Parses configuration using a key lookup function.
    ///
    /// Split out so tests can inject values without touching process
    /// environment.
    fn parse(
        get: impl Fn(&str) -> Option<String>,
        socket: &SocketPath,
        agent_uid: u32,
    ) -> Result<Self, Error> {
        let agent_bin = get(env::HATCHLING_AGENT_BIN.name)
            .ok_or(Error::MissingConfig("HATCHLING_AGENT_BIN"))?;
        let agent_bin = PathBuf::from(agent_bin);
        if !agent_bin.is_absolute() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_AGENT_BIN",
                reason: format!(
                    "agent binary path must be absolute: {}",
                    agent_bin.display()
                ),
            });
        }
        if !agent_bin.is_file() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_AGENT_BIN",
                reason: format!("agent binary not found: {}", agent_bin.display()),
            });
        }
        let cell_id =
            get(env::HATCHLING_CELL_ID.name).ok_or(Error::MissingConfig("HATCHLING_CELL_ID"))?;
        if cell_id.trim().is_empty() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_ID",
                reason: String::from("cell id must not be blank"),
            });
        }
        let backend = hatchling_isolation::platform();
        let ids = CellIds::parse(&get)?;
        let principal = match get(env::HATCHLING_CELL_UID_BASE.name) {
            Some(raw) => Some(raw.parse::<u32>().map_err(|_| Error::InvalidConfig {
                key: "HATCHLING_CELL_UID_BASE",
                reason: String::from("expected a kernel uid as a base-10 u32"),
            })?),
            None => None,
        };
        let count = match get(env::HATCHLING_CELL_UID_COUNT.name) {
            Some(raw) => Some(raw.parse::<u32>().map_err(|_| Error::InvalidConfig {
                key: "HATCHLING_CELL_UID_COUNT",
                reason: String::from("expected a count as a base-10 u32"),
            })?),
            None => None,
        };
        let identity = cell_identity(&backend, ids, principal, count)?;
        let (uid_base, uid_count) = (identity.principal, identity.count);
        // The peer gate compares SO_PEERCRED against HATCHLING_AGENT_UID
        // with exact equality: if the map base drifted from it, the
        // spawned agent could never authenticate. Fail closed here,
        // before any host state changes.
        if agent_uid != uid_base {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_UID_BASE",
                reason: format!(
                    "id-map base {uid_base} does not match the broker's HATCHLING_AGENT_UID \
                     {agent_uid}: SO_PEERCRED would report {uid_base} and the peer gate would \
                     reject the agent"
                ),
            });
        }
        let socket_path = socket.as_path().to_path_buf();
        let socket_dir = socket
            .as_path()
            .parent()
            .ok_or_else(|| Error::InvalidConfig {
                key: "HATCHLING_BROKER_SOCKET",
                reason: String::from("broker socket path has no parent directory"),
            })?
            .to_path_buf();
        let socket_name = socket
            .as_path()
            .file_name()
            .ok_or_else(|| Error::InvalidConfig {
                key: "HATCHLING_BROKER_SOCKET",
                reason: String::from("broker socket path has no file name"),
            })?;
        let broker_uid_in_cell = backend.broker_in_cell(&identity)?;
        Ok(Self {
            uid_base,
            uid_count,
            gid_base: (ids == CellIds::Subordinate).then_some(identity.group),
            ids,
            agent_bin,
            cell_id,
            socket_dir,
            socket_path,
            in_cell_socket: format!("{ENDPOINT_DIR}/{}", socket_name.to_string_lossy()),
            broker_uid_in_cell,
            // Agent cells never carry skill code; skill cells set this
            // via `SkillCellConfig::new`.
            skill_code: None,
            scheduled: None,
            harness: None,
            limits: CellLimits::parse(&get)?,
            cgroup: cgroup_parent(&get)?,
            connected: false,
        })
    }
}

/// Spawns the cell `config` describes and runs its program inside.
///
/// The broker's decisions become a [`CellSpec`] ([`cell_spec`]); the
/// platform's isolation backend builds exactly that cell and runs the
/// program as PID 2 under Hatchling's init, confined (on Linux) by its namespaces, a
/// read-only bind-mounted root, the seccomp allowlist, and
/// `NoNewPrivs`. The returned handle must be [`CellHandle::wait`]ed
/// (directly, or by a reaper thread) to avoid a zombie and to clean up
/// the staging directory.
///
/// # Errors
///
/// Returns [`Error`] if the endpoint handover, the identity, the view,
/// or any spawn step fails. Failures before the program runs are
/// fail-closed: nothing runs in a half-built cell.
pub fn spawn_cell(config: &CellConfig, log_level: LogLevel) -> Result<CellHandle, Error> {
    let cell = hatchling_isolation::platform().isolate(&cell_spec(config, log_level))?;
    Ok(CellHandle { cell })
}

/// A spawned cell under the backend's supervision: bounded waits, and
/// kill-on-drop. Dropping the handle without [`CellHandle::wait`]ing
/// ends the cell — no orphans — and removes its staging directory.
#[derive(Debug)]
pub struct CellHandle {
    cell: <hatchling_isolation::Platform as IsolationBackend>::Cell,
}

impl CellHandle {
    /// The cell's pid, for diagnostics and logging only. Never a signal
    /// target: the backend signals through its own supervision.
    #[must_use]
    pub fn child_pid(&self) -> i32 {
        self.cell.pid()
    }

    /// The cell's relay listener, once: `None` for a cell
    /// without a network, or once taken.
    pub fn relay(&mut self) -> Option<std::net::TcpListener> {
        self.cell.relay()
    }

    /// The broker's end of a connected cell's connection, once: `None`
    /// for a cell that isn't connected, or once taken.
    pub fn connection(&mut self) -> Option<std::os::unix::net::UnixStream> {
        self.cell.connection()
    }

    /// Waits for the cell to end, returning the platform's raw status
    /// (on Linux the reaper's `waitpid` status: the agent's exit code,
    /// or `124` if the agent exceeded the maximum runtime). The exit
    /// code itself is [`CellHandle::wait_exit`]'s.
    ///
    /// # Errors
    ///
    /// As [`CellHandle::wait_exit`].
    pub fn wait(self) -> Result<i32, Error> {
        self.wait_exit().map(|exit| exit.status)
    }

    /// Waits for the cell to end, within the backend's bound on a
    /// cell's runtime, and reports how it ended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CellSpawn`] if the cell reported a setup
    /// failure, or [`Error::CellWait`] if waiting fails or the maximum
    /// runtime is exceeded.
    pub fn wait_exit(self) -> Result<Exit, Error> {
        Ok(self.cell.wait()?)
    }
}

/// A helper process under the platform backend's supervision.
pub(crate) type Process = <hatchling_isolation::Platform as IsolationBackend>::Process;

/// Where the cell program starts, when not at `/`: a harness cell
/// works in its work tree, `/work`.
fn cell_workdir(config: &CellConfig) -> Option<&'static str> {
    config.is_harness_cell().then_some(IN_CELL_WORK)
}

/// The filter class the cell program runs under: a real CLI's is the CLI
/// class, another harness cell's adds the file calls it needs to
/// edit its work tree, and every other cell gets the agent's.
const fn cell_filter(config: &CellConfig) -> Filter {
    match &config.harness {
        Some(HarnessDirs {
            network: Some(_), ..
        }) => Filter::Cli,
        Some(_) => Filter::WorkTree,
        None => Filter::Program,
    }
}

/// A harness cell's network, if it has one.
fn harness_network(config: &CellConfig) -> Option<&HarnessNetwork> {
    config
        .harness
        .as_ref()
        .and_then(|harness| harness.network.as_ref())
}

/// The cell program's environment and argv, exactly as it runs with
/// them.
///
/// - **An agent cell** gets its cell id, the in-cell broker socket, the
///   broker's in-cell uid, and the log level. A connected one gets no
///   socket: the spawn tells it where its connection is
///   ([`hatchling_isolation::CONNECTION_FD_VAR`]).
/// - **A skill cell** learns nothing about the broker: it has no socket
///   to reach, and gets no address or uid to try.
/// - **A run cell** is an agent cell that adds exactly its run's
///   variables ([`ScheduledRun::env`]) and runs the agent's `run`
///   subcommand on its task.
/// - **A harness cell** is a skill cell whose `HOME` and `TMPDIR` are its
///   scratch directory, and whose program runs with the harness's
///   arguments. Nothing else: no vault, no broker, no host
///   variable.
fn program_env(config: &CellConfig, log_level: LogLevel) -> (Vec<String>, Vec<(String, String)>) {
    let mut env = vec![(String::from("HATCHLING_CELL_ID"), config.cell_id.clone())];
    if !config.is_skill_cell() {
        if !config.connected {
            env.push((
                String::from("HATCHLING_BROKER_SOCKET"),
                config.in_cell_socket.clone(),
            ));
        }
        env.push((
            String::from("HATCHLING_BROKER_UID"),
            config.broker_uid_in_cell.to_string(),
        ));
    }
    env.push((
        String::from("HATCHLING_LOG_LEVEL"),
        String::from(log_level_name(log_level)),
    ));
    if config.is_harness_cell() {
        for key in ["HOME", "TMPDIR"] {
            env.push((String::from(key), String::from(IN_CELL_SCRATCH)));
        }
    }
    if let Some(network) = harness_network(config) {
        env.extend(network.env.iter().cloned());
    }
    let harness_args = config.harness.as_ref().map(|harness| harness.args.clone());
    let argv = config.scheduled.as_ref().map_or_else(
        || {
            let mut argv = vec![String::from(PROGRAM)];
            argv.extend(harness_args.unwrap_or_default());
            argv
        },
        |run| {
            env.extend(
                run.env()
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value)),
            );
            run.argv().iter().map(|arg| (*arg).to_owned()).collect()
        },
    );
    (argv, env)
}

/// Canonical log-level name for the cell environment.
const fn log_level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

/// The broker's decisions for one cell, as the isolation backend takes
/// them.
///
/// - **Identity:** the configured uid base and count.
/// - **Endpoint:** agent cells that aren't connected only. A skill cell
///   runs under the same uid mapping as the agent, so a reachable socket
///   would pass the broker's peer gate and let the skill speak as the
///   agent.
/// - **Connection:** connected agent cells only: their one way to the
///   broker, which no other process holds an end of.
/// - **Exposures:** [`cell_exposures`].
/// - **Program:** the configured binary, with [`program_env`]'s argv and
///   environment, starting in [`cell_workdir`], under [`cell_filter`].
fn cell_spec(config: &CellConfig, log_level: LogLevel) -> CellSpec {
    let (argv, env) = program_env(config, log_level);
    CellSpec {
        identity: config.identity(),
        program: config.agent_bin.clone(),
        argv,
        env,
        workdir: cell_workdir(config).map(String::from),
        endpoint: (!config.is_skill_cell() && !config.connected).then(|| Endpoint {
            socket: config.socket_path.clone(),
            dir: config.socket_dir.clone(),
        }),
        connection: !config.is_skill_cell() && config.connected,
        exposures: cell_exposures(config),
        tools: harness_network(config).map_or_else(Vec::new, |network| {
            network
                .tools
                .iter()
                .map(|(source, name)| Tool {
                    source: source.clone(),
                    name: name.clone(),
                })
                .collect()
        }),
        relay: harness_network(config).map(|network| network.relay),
        filter: cell_filter(config),
        limits: config.limits.into(),
        cgroup: config.cgroup.clone(),
    }
}

/// The host directories a cell sees besides its program and endpoint:
/// a skill cell's code read-only at `/skill`, and a harness cell's work
/// tree and scratch directory, the only writable directories any cell
/// gets. Each is the broker's own path, never one a skill
/// supplied.
fn cell_exposures(config: &CellConfig) -> Vec<Exposure> {
    let mut exposures = Vec::new();
    if let Some(skill_code) = &config.skill_code {
        exposures.push(Exposure {
            source: skill_code.clone(),
            name: String::from("skill"),
            writable: false,
            label: "skill code dir",
        });
    }
    if let Some(harness) = &config.harness {
        for (dir, name) in [(&harness.work, "work"), (&harness.scratch, "scratch")] {
            exposures.push(Exposure {
                source: dir.clone(),
                name: String::from(name),
                writable: true,
                label: "harness directory",
            });
        }
    }
    exposures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn socket(path: &str) -> SocketPath {
        path.parse().unwrap()
    }

    fn lookup<'a>(vars: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| vars.get(key).map(|value| (*value).to_owned())
    }

    fn valid_vars() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("HATCHLING_AGENT_BIN", "/bin/true"),
            ("HATCHLING_CELL_ID", "cell-01"),
        ])
    }

    /// The principal the platform backend runs a default cell as.
    fn default_base() -> u32 {
        hatchling_isolation::platform()
            .identity(None, 1)
            .unwrap()
            .principal
    }

    /// The broker's own principal.
    fn owner() -> u32 {
        hatchling_isolation::platform().owner()
    }

    #[test]
    fn parses_valid_cell_config_with_defaults() {
        let vars = valid_vars();
        let config = CellConfig::parse(
            lookup(&vars),
            &socket("/tmp/hatchling-test/broker.sock"),
            default_base(),
        )
        .unwrap();
        assert_eq!(config.agent_bin, Path::new("/bin/true"));
        assert_eq!(config.cell_id, "cell-01");
        assert_eq!(config.uid_count, 1);
        assert_eq!(config.limits, CellLimits::default());
        assert_eq!(config.socket_dir, Path::new("/tmp/hatchling-test"));
        assert_eq!(config.in_cell_socket, "/sock/broker.sock");
        // Defaults depend on privilege (the backend's choice): root gets
        // nobody, unprivileged gets its own uid.
        assert_eq!(config.uid_base, default_base());
        // The broker's in-cell uid is the backend's answer for the map:
        // an unprivileged broker maps its own uid to in-cell 0.
        let backend = hatchling_isolation::platform();
        let identity = backend.identity(None, 1).unwrap();
        assert_eq!(
            config.broker_uid_in_cell,
            backend.broker_in_cell(&identity).unwrap()
        );
        if owner() != 0 {
            assert_eq!(config.broker_uid_in_cell, 0);
        }
    }

    #[test]
    fn cell_limits_default_to_bounded_processes_files_and_file_sizes() {
        assert_eq!(
            CellLimits::default(),
            CellLimits {
                processes: Some(4096),
                address_space: None,
                file_size: Some(8 << 30),
                open_files: Some(8192),
                memory: None,
                cpu_millis: None,
            }
        );
    }

    #[test]
    fn from_env_validates_real_process_settings_without_changing_the_parent_env() {
        use std::os::unix::ffi::OsStringExt as _;
        const MARKER: &str = "HATCHLING_TEST_CELL_ENV_CASE";
        if let Ok(case) = std::env::var(MARKER) {
            let parsed = CellConfig::from_env(&socket("/tmp/cell-env/broker.sock"), default_base());
            if case == "valid" {
                let parsed = parsed.unwrap();
                assert_eq!(parsed.agent_bin, PathBuf::from("/bin/true"));
                assert_eq!(parsed.cell_id, "cell-environment");
                assert_eq!(parsed.cgroup, None);
                assert_eq!(parsed.limits.memory, Some(123_456));
                assert_eq!(parsed.limits.cpu_millis, Some(1500));
            } else {
                assert!(matches!(parsed, Err(Error::InvalidConfig { key, .. }) if key == case));
            }
            return;
        }
        for case in [
            "valid",
            "HATCHLING_CELL_CGROUP",
            "HATCHLING_CELL_MAX_MEMORY",
            "HATCHLING_CELL_MAX_CPUS",
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "cell::tests::from_env_validates_real_process_settings_without_changing_the_parent_env", "--nocapture"])
                .env_clear().env(MARKER, case)
                .env("HATCHLING_AGENT_BIN", "/bin/true")
                .env("HATCHLING_CELL_ID", "cell-environment")
                .env("HATCHLING_CELL_MAX_MEMORY", "123456")
                .env("HATCHLING_CELL_MAX_CPUS", "1.5");
            if case != "valid" {
                command.env(case, std::ffi::OsString::from_vec(vec![0xff]));
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "the exact child case must run"
            );
        }
    }

    #[test]
    fn malformed_process_settings_never_select_missing_value_defaults() {
        use std::os::unix::ffi::OsStringExt as _;
        let invalid = || {
            env_value(Err(std::env::VarError::NotUnicode(
                std::ffi::OsString::from_vec(vec![0xff]),
            )))
        };
        assert_eq!(
            env_value(Ok(String::from("1.5"))),
            Some(String::from("1.5"))
        );
        assert_eq!(env_value(Err(std::env::VarError::NotPresent)), None);
        assert_eq!(
            CellLimits::parse(|_| env_value(Err(std::env::VarError::NotPresent))).unwrap(),
            CellLimits::default()
        );
        assert_eq!(
            cgroup_parent(&|_| env_value(Err(std::env::VarError::NotPresent))).unwrap(),
            None
        );
        assert!(matches!(
            cgroup_parent(&|_| invalid()),
            Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_CGROUP",
                ..
            })
        ));
        for bad_key in ["HATCHLING_CELL_MAX_MEMORY", "HATCHLING_CELL_MAX_CPUS"] {
            let parsed = CellLimits::parse(|key| if key == bad_key { invalid() } else { None });
            assert!(matches!(parsed, Err(Error::InvalidConfig { key, .. }) if key == bad_key));
        }
        let parsed = CellLimits::parse(|key| match key {
            "HATCHLING_CELL_MAX_MEMORY" => env_value(Ok(String::from("1048576"))),
            "HATCHLING_CELL_MAX_CPUS" => env_value(Ok(String::from("1.5"))),
            _ => env_value(Err(std::env::VarError::NotPresent)),
        })
        .unwrap();
        assert_eq!(parsed.memory, Some(1_048_576));
        assert_eq!(parsed.cpu_millis, Some(1500));
    }

    #[test]
    fn cpu_limits_are_exact_fractional_cores_with_checked_bounds() {
        assert_eq!(cpu_limit(None).unwrap(), None);
        assert_eq!(cpu_limit(Some("unlimited")).unwrap(), None);
        for (raw, expected) in [("1", 1000), ("1.5", 1500), ("0.01", 10), ("2.125", 2125)] {
            assert_eq!(cpu_limit(Some(raw)).unwrap(), Some(expected));
        }
        for raw in [
            "",
            "-1",
            "+1",
            "1.+1",
            "0",
            "0.009",
            "1.0001",
            "NaN",
            "inf",
            " 1",
            "1.x",
            "18446744073709551615",
            ".5",
        ] {
            assert!(cpu_limit(Some(raw)).is_err(), "{raw}");
        }
        let vars = HashMap::from([
            ("HATCHLING_CELL_MAX_CPUS", "1.5"),
            ("HATCHLING_CELL_MAX_MEMORY", "123456"),
        ]);
        let parsed = CellLimits::parse(lookup(&vars)).unwrap();
        assert_eq!(parsed.memory, Some(123_456));
        assert_eq!(parsed.cpu_millis, Some(1500));
        let limits = Limits::from(parsed);
        assert_eq!(limits.memory, parsed.memory);
        assert_eq!(limits.cpu_millis, parsed.cpu_millis);
        let vars = HashMap::from([("HATCHLING_CELL_CGROUP", "/a-delegation")]);
        assert_eq!(
            CellLimits::parse(lookup(&vars)).unwrap().memory,
            Some(hatchling_sys::cgroup::default_memory().unwrap())
        );
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "simulates SIGKILL of a helper, which cannot run its cgroup destructor"
    )]
    fn helper_cgroups_are_owned_by_the_broker_and_reach_the_cell_spec() {
        let mut cell = CellConfig::skill("cgroup-helper").unwrap();
        let (fallback, group) = cell.supervised_cgroup().unwrap();
        assert!(group.is_none());
        assert!(fallback.cgroup.is_none());
        let Some(base) = std::env::var_os("HATCHLING_TEST_CGROUP") else {
            assert!(!std::env::var("HATCHLING_REQUIRE_CGROUP").is_ok_and(|v| v == "1"));
            return;
        };
        let mut root = hatchling_sys::cgroup::CellCgroup::create(
            Path::new(&base),
            hatchling_sys::cgroup::CgroupLimits::default(),
        )
        .unwrap();
        cell.cgroup = Some(root.path().to_path_buf());
        let (scoped, mut group) = cell.supervised_cgroup().unwrap();
        let group = group.as_mut().unwrap();
        let path = group.path().to_path_buf();
        assert_ne!(scoped.cgroup, cell.cgroup);
        assert_eq!(scoped.cgroup.as_deref(), Some(group.path()));
        assert_eq!(cell_spec(&scoped, LogLevel::Error).cgroup, scoped.cgroup);
        let nested = hatchling_sys::cgroup::CellCgroup::create(
            group.path(),
            hatchling_sys::cgroup::CgroupLimits::default(),
        )
        .unwrap();
        let nested_path = nested.path().to_path_buf();
        std::mem::forget(nested);
        group.finish().unwrap();
        assert!(!nested_path.exists());
        assert!(!path.exists());
        root.finish().unwrap();
    }

    #[test]
    fn configured_cgroups_fail_closed_and_unset_uses_the_fallback() {
        assert_eq!(cgroup_parent(&|_| None).unwrap(), None);
        for raw in ["", "relative", "/tmp", "/no-such-hatchling-cgroup"] {
            let vars = HashMap::from([("HATCHLING_CELL_CGROUP", raw)]);
            assert!(matches!(
                cgroup_parent(&lookup(&vars)),
                Err(Error::InvalidConfig {
                    key: "HATCHLING_CELL_CGROUP",
                    ..
                })
            ));
        }
    }

    #[test]
    fn a_cell_limit_is_a_positive_number_or_unlimited() {
        let vars = HashMap::from([
            ("HATCHLING_AGENT_BIN", "/bin/true"),
            ("HATCHLING_CELL_ID", "cell-01"),
            ("HATCHLING_CELL_MAX_PROCESSES", "64"),
            ("HATCHLING_CELL_MAX_ADDRESS_SPACE", "4294967296"),
            ("HATCHLING_CELL_MAX_FILE_SIZE", "unlimited"),
        ]);
        let expected = CellLimits {
            processes: Some(64),
            address_space: Some(1 << 32),
            file_size: None,
            open_files: Some(8192),
            memory: None,
            cpu_millis: None,
        };
        assert_eq!(CellLimits::parse(lookup(&vars)).unwrap(), expected);
        let config =
            CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
                .unwrap();
        assert_eq!(config.limits, expected);
        for bad in [
            "0",
            "-1",
            "lots",
            " 64",
            "18446744073709551616",
            "Unlimited",
            "",
        ] {
            let vars = HashMap::from([("HATCHLING_CELL_MAX_OPEN_FILES", bad)]);
            assert!(
                matches!(
                    CellLimits::parse(lookup(&vars)),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_CELL_MAX_OPEN_FILES",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        let vars = HashMap::from([("HATCHLING_CELL_MAX_OPEN_FILES", "1")]);
        let key = "HATCHLING_CELL_MAX_OPEN_FILES";
        assert_eq!(limit(&lookup(&vars), key, Some(7)).unwrap(), Some(1));
        assert_eq!(limit(&lookup(&vars), "unset", Some(7)).unwrap(), Some(7));
        assert_eq!(limit(&lookup(&vars), "unset", None).unwrap(), None);
    }

    #[test]
    fn missing_agent_bin_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_AGENT_BIN");
        let err = CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
            .unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_AGENT_BIN")));
    }

    #[test]
    fn missing_cell_id_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_CELL_ID");
        let err = CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
            .unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_CELL_ID")));
    }

    #[test]
    fn relative_agent_bin_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_AGENT_BIN", "relative/agent");
        let err = CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
            .unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_AGENT_BIN"));
    }

    #[test]
    fn missing_agent_binary_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_AGENT_BIN", "/nonexistent/agent-binary");
        let err = CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
            .unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_AGENT_BIN"));
    }

    #[test]
    fn invalid_uid_base_is_an_error() {
        for raw in ["not-a-uid", "-1", ""] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_CELL_UID_BASE", raw);
            let err =
                CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), default_base())
                    .unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_CELL_UID_BASE"),
                "raw value {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn root_mapping_cell_root_to_host_root_is_an_error() {
        // Only meaningful when running as root; unprivileged the map
        // validation rejects base 0 for a different reason.
        if owner() != 0 {
            return;
        }
        let mut vars = valid_vars();
        vars.insert("HATCHLING_CELL_UID_BASE", "0");
        // Pass agent_uid 0 explicitly: the uid-base/agent-uid
        // consistency check must not mask the map-policy rejection.
        let err = CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), 0).unwrap_err();
        assert!(matches!(err, Error::CellMap { .. }));
    }

    #[test]
    fn agent_uid_mismatch_with_uid_base_is_an_error() {
        // SO_PEERCRED reports the map base; the broker's peer gate
        // expects HATCHLING_AGENT_UID. If they differ the agent could
        // never authenticate, so config parsing fails closed.
        let vars = valid_vars();
        let base = default_base();
        let err =
            CellConfig::parse(lookup(&vars), &socket("/tmp/x/broker.sock"), base + 1).unwrap_err();
        assert!(
            matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_CELL_UID_BASE")
        );
    }

    /// `HATCHLING_CELL_IDS` as `value`, nothing else.
    fn cell_ids_set(value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |key| (key == "HATCHLING_CELL_IDS").then(|| value.to_owned())
    }

    #[test]
    fn where_cells_get_their_ids_is_own_or_subordinate() {
        assert_eq!(CellIds::parse(|_| None).unwrap(), CellIds::Own);
        assert_eq!(CellIds::parse(cell_ids_set("own")).unwrap(), CellIds::Own);
        assert_eq!(
            CellIds::parse(cell_ids_set("subordinate")).unwrap(),
            CellIds::Subordinate
        );
        for wrong in ["Subordinate", "", "subid"] {
            assert!(
                matches!(
                    CellIds::parse(cell_ids_set(wrong)),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_CELL_IDS",
                        ..
                    })
                ),
                "{wrong:?}"
            );
        }
    }

    /// Every kind of cell takes the owner's subordinate ids when told
    /// to, and a base or count beside them is refused; where the owner
    /// has none, the choice is refused, not quietly swapped for the
    /// broker's own map.
    #[test]
    fn a_subordinate_cell_takes_the_owners_ids_and_no_base() {
        let backend = hatchling_isolation::platform();
        assert_eq!(
            cell_identity(&backend, CellIds::Own, None, None).unwrap(),
            backend.identity(None, 1).unwrap()
        );
        assert_eq!(
            cell_identity(&backend, CellIds::Own, Some(default_base()), Some(1)).unwrap(),
            backend.identity(Some(default_base()), 1).unwrap()
        );
        for (principal, count) in [(Some(100_000), None), (None, Some(1))] {
            assert!(matches!(
                cell_identity(&backend, CellIds::Subordinate, principal, count),
                Err(Error::InvalidConfig {
                    key: "HATCHLING_CELL_IDS",
                    ..
                })
            ));
        }
        let mut vars = valid_vars();
        vars.insert("HATCHLING_CELL_IDS", "subordinate");
        let owners = match backend.subordinate_identity() {
            Ok(owners) => owners,
            Err(_) => {
                assert!(matches!(
                    cell_identity(&backend, CellIds::Subordinate, None, None),
                    Err(Error::CellMap { .. })
                ));
                assert!(CellConfig::skill_with("skill-sub", cell_ids_set("subordinate")).is_err());
                return;
            }
        };
        assert_eq!(owners.mapping, Mapping::Subordinate);
        assert_eq!(
            cell_identity(&backend, CellIds::Subordinate, None, None).unwrap(),
            owners
        );
        // A skill or harness cell.
        let skill = CellConfig::skill_with("skill-sub", cell_ids_set("subordinate")).unwrap();
        assert_eq!(skill.identity(), owners);
        assert_eq!(
            skill.broker_uid_in_cell,
            backend.broker_in_cell(&owners).unwrap()
        );
        // The agent's cell and a run's, whose agent uid must be the
        // subordinate one.
        let socket = socket("/tmp/x/broker.sock");
        let agent = CellConfig::parse(lookup(&vars), &socket, owners.principal).unwrap();
        assert_eq!(agent.identity(), owners);
        let err = CellConfig::parse(lookup(&vars), &socket, default_base()).unwrap_err();
        assert!(
            matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_CELL_UID_BASE"),
            "{err:?}"
        );
        vars.insert("HATCHLING_CELL_UID_BASE", "100000");
        assert!(matches!(
            CellConfig::parse(lookup(&vars), &socket, owners.principal),
            Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_IDS",
                ..
            })
        ));
    }

    #[test]
    fn a_configs_identity_is_its_map_and_where_its_ids_come_from() {
        let mut config = CellConfig::skill("ids").unwrap();
        assert_eq!(config.ids, CellIds::Own);
        assert_eq!(config.gid_base, None);
        assert_eq!(
            config.identity(),
            CellIdentity::direct(config.uid_base, config.uid_count)
        );
        config.ids = CellIds::Subordinate;
        config.uid_base = 100_000;
        config.gid_base = Some(200_000);
        let subordinate = CellIdentity {
            principal: 100_000,
            group: 200_000,
            count: config.uid_count,
            mapping: Mapping::Subordinate,
        };
        assert_eq!(config.identity(), subordinate);
        assert_eq!(cell_spec(&config, LogLevel::Error).identity, subordinate);
        // The spawner helpers get it whole, through serde.
        let carried: CellConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(carried.identity(), subordinate);
        // A config written before the ids were carried is its own map.
        let mut value = serde_json::to_value(&config).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("ids");
        fields.remove("gid_base");
        let older: CellConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            older.identity(),
            CellIdentity::direct(100_000, config.uid_count)
        );
    }

    /// WO-014, end to end through the broker's own spawn: a skill cell on
    /// the owner's subordinate ids runs as them, with no groups, as the
    /// host sees it. Skips where no cell can run on them (an owner with
    /// root, `no_new_privs`, no range), unless
    /// `HATCHLING_REQUIRE_SUBID=1`.
    #[test]
    fn a_skill_cell_on_subordinate_ids_runs_as_them() {
        let required = std::env::var("HATCHLING_REQUIRE_SUBID").is_ok_and(|v| v == "1");
        let new_privs = std::fs::read_to_string("/proc/self/status").is_ok_and(|status| {
            status
                .lines()
                .any(|line| line.split_whitespace().eq(["NoNewPrivs:", "0"]))
        });
        let config = new_privs
            .then(|| CellConfig::skill_with("sub-x", cell_ids_set("subordinate")).ok())
            .flatten();
        let Some(mut config) = config else {
            assert!(!required, "HATCHLING_REQUIRE_SUBID=1, but no subordinate cell here");
            return;
        };
        let code = std::env::temp_dir().join(format!("hatchling-sub-{}", std::process::id()));
        std::fs::create_dir_all(&code).unwrap();
        config.agent_bin = ["/usr/bin/gnusleep", "/bin/sleep"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.exists())
            .unwrap();
        config.skill_code = Some(code.clone());
        let spawned = spawn_cell(&config, LogLevel::Error);
        let _ = std::fs::remove_dir_all(&code);
        let handle = match spawned {
            Ok(handle) => handle,
            Err(error) => {
                assert!(!required, "HATCHLING_REQUIRE_SUBID=1 but no cell: {error}");
                return;
            }
        };
        let status = |field: &str| -> Vec<String> {
            std::fs::read_to_string(format!("/proc/{}/status", handle.child_pid()))
                .unwrap_or_default()
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{field}:")))
                .map(|value| value.split_whitespace().map(String::from).collect())
                .unwrap_or_default()
        };
        let (uid, gid) = (config.uid_base.to_string(), config.identity().group.to_string());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while status("Uid") != vec![uid.clone(); 4] && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let seen = (status("Uid"), status("Gid"), status("Groups"));
        drop(handle);
        assert_eq!(
            seen,
            (vec![uid; 4], vec![gid; 4], Vec::<String>::new()),
            "the cell's uid, gid and groups, from the host"
        );
    }

    #[test]
    fn log_level_names_cover_every_variant() {
        // Pins the match exhaustiveness: a new LogLevel variant fails
        // compilation here, forcing a conscious env spelling.
        assert_eq!(log_level_name(LogLevel::Error), "error");
        assert_eq!(log_level_name(LogLevel::Warn), "warn");
        assert_eq!(log_level_name(LogLevel::Info), "info");
        assert_eq!(log_level_name(LogLevel::Debug), "debug");
        assert_eq!(log_level_name(LogLevel::Trace), "trace");
    }

    #[test]
    fn parse_test_wires_agent_bin_and_cell_id() {
        // Mutants: delete match arm "HATCHLING_AGENT_BIN" /
        // "HATCHLING_CELL_ID" in parse_test. Each arm must actually
        // feed its value into the config.
        let dir = std::env::temp_dir().join(format!("hatchling-parse-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = default_base();
        let config =
            CellConfig::parse_test(Path::new("/bin/true"), "cell-parse-01", &dir, base).unwrap();
        assert_eq!(config.agent_bin, Path::new("/bin/true"));
        assert_eq!(config.cell_id, "cell-parse-01");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_test_err_wires_uid_base() {
        // Mutant: delete match arm "HATCHLING_CELL_UID_BASE" in
        // parse_test_err. The arm must inject the uid_base arg into
        // the parse; without it, the parse uses the default base.
        //
        // Root strategy: pass a uid_base that differs from the
        // default but matches agent_uid, so parse SUCCEEDS with the
        // arm present (parse_test_err returns Err("expected parse to
        // fail")) but FAILS with the arm deleted (default base
        // mismatches agent_uid, parse_test_err returns Ok).
        //
        // Unprivileged strategy: the only valid base is the euid
        // itself, which equals the default — so instead pass an
        // INVALID base with agent_uid = euid. With the arm present,
        // the idmap validator rejects it (parse fails, parse_test_err
        // returns Ok); with the arm deleted, the default base matches
        // agent_uid, parse succeeds, parse_test_err returns Err.
        let euid = owner();
        let dir = std::env::temp_dir().join(format!("hatchling-parse-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = if euid == 0 {
            // 50000 differs from the root default (65534) and is
            // valid for a root broker.
            let base = 50000u32;
            CellConfig::parse_test_err(
                Path::new("/bin/true"),
                "cell-parse-02",
                &dir,
                base,
                base, // matches: parse must succeed with the arm present
            )
        } else {
            CellConfig::parse_test_err(
                Path::new("/bin/true"),
                "cell-parse-02",
                &dir,
                50000, // invalid for an unprivileged broker
                euid,  // matches the default base if the arm is deleted
            )
        };
        let _ = std::fs::remove_dir_all(&dir);
        if euid == 0 {
            assert!(
                result.is_err(),
                "parse with matching uid_base/agent_uid must succeed (arm must inject the base), got {result:?}"
            );
        } else {
            assert!(
                result.is_ok(),
                "parse with an invalid uid_base must fail (arm must inject the base), got {result:?}"
            );
        }
    }

    #[test]
    fn parse_test_err_wires_agent_bin_and_cell_id() {
        // Mutants: delete match arm "HATCHLING_AGENT_BIN" or
        // "HATCHLING_CELL_ID" in parse_test_err. Each arm must feed
        // its value into the parse: with both present and valid, parse
        // SUCCEEDS (parse_test_err returns Err("expected parse to
        // fail")); with either deleted, the key is missing and parse
        // fails (parse_test_err returns Ok). Needs no privilege: the
        // base matches agent_uid in both privilege modes.
        let euid = owner();
        // A root broker must not map base 0; 50000 is valid there.
        // Unprivileged, the euid is the only valid base.
        let (base, agent) = if euid == 0 {
            (50000u32, 50000u32)
        } else {
            (euid, euid)
        };
        let dir = std::env::temp_dir().join(format!("hatchling-parse-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result =
            CellConfig::parse_test_err(Path::new("/bin/true"), "cell-parse-03", &dir, base, agent);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            result.is_err(),
            "parse with all keys wired must succeed, got {result:?}"
        );
    }

    #[test]
    fn a_skill_base_config_has_the_default_map_and_no_socket() {
        let config = CellConfig::skill("skill-x").unwrap();
        assert_eq!(config.uid_base, default_base());
        assert_eq!(config.uid_count, 1);
        assert_eq!(config.cell_id, "skill-x");
        assert_eq!(config.socket_dir, PathBuf::new());
        assert_eq!(config.socket_path, PathBuf::new());
        assert!(config.in_cell_socket.is_empty());
        // The broker's in-cell uid is computed exactly as for the agent.
        let agent = CellConfig::parse_test(
            Path::new("/bin/true"),
            "agent-x",
            Path::new("/tmp"),
            default_base(),
        )
        .unwrap();
        assert_eq!(config.broker_uid_in_cell, agent.broker_uid_in_cell);
        // Not a skill cell until the verified code is set.
        assert!(!config.is_skill_cell());
        let mut skill = config;
        skill.skill_code = Some(PathBuf::from("/tmp/code"));
        assert!(skill.is_skill_cell());
    }

    #[test]
    fn a_cell_config_round_trips_through_its_spec() {
        let mut config = CellConfig::skill("skill-y").unwrap();
        config.agent_bin = PathBuf::from("/tmp/code/skill");
        config.skill_code = Some(PathBuf::from("/tmp/code"));
        let spec = serde_json::to_string(&config).unwrap();
        let back: CellConfig = serde_json::from_str(&spec).unwrap();
        assert_eq!(format!("{back:?}"), format!("{config:?}"));
    }

    #[test]
    fn an_agent_cells_spec_is_the_brokers_decisions() {
        let dir = std::env::temp_dir().join(format!("hatchling-spec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config =
            CellConfig::parse_test(Path::new("/bin/true"), "cell-spec-01", &dir, default_base())
                .unwrap();
        let spec = cell_spec(&config, LogLevel::Info);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            spec.identity,
            CellIdentity::direct(config.uid_base, config.uid_count)
        );
        assert_eq!(spec.program, Path::new("/bin/true"));
        assert_eq!(
            (spec.argv.clone(), spec.env.clone()),
            program_env(&config, LogLevel::Info)
        );
        assert_eq!(
            spec.endpoint,
            Some(Endpoint {
                socket: dir.join("broker.sock"),
                dir: dir.clone(),
            })
        );
        assert!(spec.exposures.is_empty());
        assert_eq!(spec.workdir, None);
        assert_eq!(spec.filter, Filter::Program);
        assert_eq!(
            spec.limits,
            Limits {
                processes: Some(4096),
                address_space: None,
                file_size: Some(8 << 30),
                open_files: Some(8192),
                memory: None,
                cpu_millis: None,
            }
        );
    }

    #[test]
    fn a_skill_cells_spec_has_its_code_and_no_endpoint() {
        let mut config = CellConfig::skill("skill-spec").unwrap();
        config.agent_bin = PathBuf::from("/tmp/code/skill");
        config.skill_code = Some(PathBuf::from("/tmp/code"));
        let spec = cell_spec(&config, LogLevel::Error);
        assert_eq!(spec.endpoint, None);
        assert_eq!(spec.program, Path::new("/tmp/code/skill"));
        assert_eq!(
            spec.exposures,
            [Exposure {
                source: PathBuf::from("/tmp/code"),
                name: String::from("skill"),
                writable: false,
                label: "skill code dir",
            }]
        );
    }

    /// A real cell through the broker's own call: the program runs, and
    /// both waits report how it ended. On a box that cannot build a cell
    /// the spawn fails: an honest skip, unless `HATCHLING_REQUIRE_CELL=1`.
    #[test]
    fn a_spawned_cell_reports_its_exit_both_ways() {
        let code = std::env::temp_dir().join(format!("hatchling-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&code).unwrap();
        let mut config = CellConfig::skill("spawn-x").unwrap();
        config.agent_bin = PathBuf::from("/bin/true");
        config.skill_code = Some(code.clone());
        let mut handle = match spawn_cell(&config, LogLevel::Error) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&code);
                let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
                assert!(!required, "HATCHLING_REQUIRE_CELL=1 but no cell: {error}");
                return;
            }
        };
        assert!(handle.child_pid() > 1);
        assert!(handle.relay().is_none(), "a cell without a network");
        let exit = handle.wait_exit().unwrap();
        assert_eq!(exit.code, 0);
        let handle = spawn_cell(&config, LogLevel::Error).unwrap();
        assert_eq!(handle.wait().unwrap(), exit.status);
        let _ = std::fs::remove_dir_all(&code);
    }

    #[test]
    fn a_failed_spawn_is_the_brokers_error() {
        // An agent cell whose broker socket is missing is refused before
        // anything is built.
        let dir = std::env::temp_dir().join(format!("hatchling-nosock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = CellConfig::parse_test(
            Path::new("/bin/true"),
            "spawn-missing",
            &dir,
            default_base(),
        )
        .unwrap();
        let error = spawn_cell(&config, LogLevel::Error).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&error, Error::CellSpawn { reason } if reason.starts_with("cannot hand the broker socket ")),
            "{error:?}"
        );
    }
}

#[cfg(test)]
mod run_cell_tests {
    use super::*;

    fn run() -> ScheduledRun {
        ScheduledRun {
            task: String::from("check the build"),
            missed: true,
            due: 1_790_000_000,
            model_endpoint: Some(String::from("https://models.example.com/v1")),
            model_name: Some(String::from("m-1")),
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            depth: 2,
            event: Some(hatchling_core::HookSource::ChannelMessage {
                channel: String::from("local"),
            }),
            max_steps: None,
            subagent: false,
            chat: false,
        }
    }

    fn agent_cell() -> CellConfig {
        let mut config = CellConfig::skill("cell-a").unwrap();
        config.in_cell_socket = String::from("/sock/broker.sock");
        config
    }

    fn keys(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(key, _)| key.as_str()).collect()
    }

    #[test]
    fn an_agent_cell_gets_the_broker_address_and_no_run() {
        let (argv, env) = program_env(&agent_cell(), LogLevel::Warn);
        assert_eq!(argv, [PROGRAM]);
        assert_eq!(
            keys(&env),
            [
                "HATCHLING_CELL_ID",
                "HATCHLING_BROKER_SOCKET",
                "HATCHLING_BROKER_UID",
                "HATCHLING_LOG_LEVEL"
            ]
        );
        assert!(env.contains(&(String::from("HATCHLING_LOG_LEVEL"), String::from("warn"))));
        assert!(env.contains(&(
            String::from("HATCHLING_BROKER_SOCKET"),
            String::from("/sock/broker.sock")
        )));
    }

    #[test]
    fn a_connected_cell_gets_its_connection_and_no_socket() {
        let mut cell = agent_cell();
        cell.connected = true;
        let (_, env) = program_env(&cell, LogLevel::Warn);
        assert_eq!(
            keys(&env),
            [
                "HATCHLING_CELL_ID",
                "HATCHLING_BROKER_UID",
                "HATCHLING_LOG_LEVEL"
            ]
        );
        let spec = cell_spec(&cell, LogLevel::Warn);
        assert!(spec.connection, "a connected agent cell has its connection");
        assert_eq!(spec.endpoint, None, "and no socket");
        // An agent cell that isn't connected reaches the socket instead.
        let plain = cell_spec(&agent_cell(), LogLevel::Warn);
        assert!(!plain.connection);
        assert!(plain.endpoint.is_some());
        // A skill cell reaches neither, whatever it says.
        cell.skill_code = Some(PathBuf::from("/tmp/code"));
        let skill = cell_spec(&cell, LogLevel::Warn);
        assert!(!skill.connection);
        assert_eq!(skill.endpoint, None);
    }

    #[test]
    fn a_skill_cell_learns_nothing_about_the_broker() {
        let mut skill = agent_cell();
        skill.skill_code = Some(PathBuf::from("/tmp/code"));
        let (argv, env) = program_env(&skill, LogLevel::Error);
        assert_eq!(argv, [PROGRAM]);
        assert_eq!(keys(&env), ["HATCHLING_CELL_ID", "HATCHLING_LOG_LEVEL"]);
    }

    #[test]
    fn a_run_cell_runs_its_task_with_exactly_its_run_variables() {
        let mut cell = agent_cell();
        cell.scheduled = Some(run());
        let (argv, env) = program_env(&cell, LogLevel::Error);
        assert_eq!(argv, [PROGRAM, "run", "check the build"]);
        assert_eq!(
            keys(&env),
            [
                "HATCHLING_CELL_ID",
                "HATCHLING_BROKER_SOCKET",
                "HATCHLING_BROKER_UID",
                "HATCHLING_LOG_LEVEL",
                "HATCHLING_RUN_MISSED",
                "HATCHLING_RUN_DUE",
                "HATCHLING_RUN_DEPTH",
                "HATCHLING_RUN_EVENT",
                "HATCHLING_MODEL_ENDPOINT",
                "HATCHLING_MODEL_NAME"
            ]
        );
        assert!(env.contains(&(String::from("HATCHLING_RUN_DEPTH"), String::from("2"))));
        assert!(env.contains(&(
            String::from("HATCHLING_RUN_EVENT"),
            String::from("channel-message:local")
        )));
        assert!(env.contains(&(String::from("HATCHLING_RUN_MISSED"), String::from("1"))));
        assert!(env.contains(&(
            String::from("HATCHLING_RUN_DUE"),
            String::from("1790000000")
        )));
        let on_time = ScheduledRun {
            missed: false,
            model_endpoint: None,
            model_name: None,
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            depth: 0,
            event: None,
            ..run()
        };
        assert_eq!(
            on_time.env(),
            [
                ("HATCHLING_RUN_MISSED", String::from("0")),
                ("HATCHLING_RUN_DUE", String::from("1790000000")),
                ("HATCHLING_RUN_DEPTH", String::from("0"))
            ]
        );
    }

    #[test]
    fn a_runs_argv_is_the_agents_run_subcommand() {
        assert_eq!(run().argv(), [PROGRAM, "run", "check the build"]);
    }

    #[test]
    fn a_subagents_run_carries_its_budget_and_its_mode() {
        let child = ScheduledRun {
            missed: false,
            model_endpoint: None,
            model_name: None,
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            depth: 0,
            event: None,
            max_steps: Some(4),
            subagent: true,
            ..run()
        };
        assert_eq!(
            child.env(),
            [
                ("HATCHLING_RUN_MISSED", String::from("0")),
                ("HATCHLING_RUN_DUE", String::from("1790000000")),
                ("HATCHLING_RUN_DEPTH", String::from("0")),
                ("HATCHLING_MODEL_MAX_STEPS", String::from("4")),
                ("HATCHLING_RUN_SUBAGENT", String::from("1")),
            ]
        );
    }

    #[test]
    fn a_chat_turns_run_says_so_and_nothing_else_does() {
        let turn = ScheduledRun {
            chat: true,
            ..run()
        };
        assert!(
            turn.env()
                .contains(&("HATCHLING_RUN_CHAT", String::from("1")))
        );
        assert!(
            !run()
                .env()
                .iter()
                .any(|(name, _)| *name == "HATCHLING_RUN_CHAT"),
            "background work speaks with the worker's prompt"
        );
    }

    #[test]
    fn a_run_cell_is_configured_for_its_own_socket_only() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-test-runcell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("agent");
        std::fs::write(&bin, b"#!").unwrap();
        let socket: SocketPath = dir
            .join("run-1")
            .join("broker.sock")
            .to_string_lossy()
            .parse()
            .unwrap();
        let uid = hatchling_isolation::platform()
            .identity(None, 1)
            .unwrap()
            .principal;
        let cell = CellConfig::for_run(&bin, "run-1", &socket, uid, run()).unwrap();
        assert_eq!(cell.socket_dir, dir.join("run-1"));
        assert_eq!(cell.socket_path, dir.join("run-1").join("broker.sock"));
        assert_eq!(cell.in_cell_socket, "/sock/broker.sock");
        assert_eq!(cell.cell_id, "run-1");
        assert_eq!(cell.scheduled, Some(run()));
        assert!(!cell.is_skill_cell());
        let nul = ScheduledRun {
            task: String::from("a\0b"),
            ..run()
        };
        assert!(matches!(
            CellConfig::for_run(&bin, "run-1", &socket, uid, nul),
            Err(Error::InvalidConfig {
                key: "scheduled run",
                ..
            })
        ));
        // The spec round-trips, run included.
        let back: CellConfig =
            serde_json::from_str(&serde_json::to_string(&cell).unwrap()).unwrap();
        assert_eq!(back.scheduled, Some(run()));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn serialized_runs_default_only_when_mode_is_absent_and_emit_disabled() {
        use hatchling_core::ModelReasoning;
        let mut value = serde_json::to_value(run()).unwrap();
        value.as_object_mut().unwrap().remove("model_reasoning");
        let old: ScheduledRun = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(old.model_reasoning, ModelReasoning::ProviderDefault);
        assert!(
            !old.env()
                .iter()
                .any(|(key, _)| *key == "HATCHLING_MODEL_REASONING")
        );
        value.as_object_mut().unwrap().insert(
            String::from("model_reasoning"),
            serde_json::json!("disabled"),
        );
        let disabled: ScheduledRun = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(disabled.model_reasoning, ModelReasoning::Disabled);
        let mut expected = old.env();
        expected.push(("HATCHLING_MODEL_REASONING", String::from("disabled")));
        assert_eq!(disabled.env(), expected);
        for invalid in [
            serde_json::json!("off"),
            serde_json::json!(""),
            serde_json::json!(null),
            serde_json::json!(false),
        ] {
            value
                .as_object_mut()
                .unwrap()
                .insert(String::from("model_reasoning"), invalid);
            assert!(serde_json::from_value::<ScheduledRun>(value.clone()).is_err());
        }
        let cell = CellConfig {
            scheduled: Some(disabled),
            ..agent_cell()
        };
        let (_, actual) = program_env(&cell, LogLevel::Error);
        assert_eq!(
            actual
                .iter()
                .filter(|(key, _)| key == "HATCHLING_MODEL_REASONING")
                .collect::<Vec<_>>(),
            [&(
                String::from("HATCHLING_MODEL_REASONING"),
                String::from("disabled")
            )]
        );
    }
}

#[cfg(test)]
mod harness_cell_tests {
    use super::*;

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hatchling-harness-cell-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["code", "work", "scratch", "root"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A skill cell running this test binary, and its harness twin.
    fn cells(scratch: &Scratch) -> (CellConfig, CellConfig) {
        let mut skill = CellConfig::skill("harness").unwrap();
        skill.agent_bin = std::env::current_exe().unwrap();
        skill.skill_code = Some(scratch.0.join("code"));
        let mut harness = skill.clone();
        harness.harness = Some(HarnessDirs {
            work: scratch.0.join("work"),
            scratch: scratch.0.join("scratch"),
            args: vec![String::from("-p"), String::from("Go.")],
            network: None,
        });
        (skill, harness)
    }

    /// The harness twin with a network.
    fn networked(harness: &CellConfig) -> CellConfig {
        let mut networked = harness.clone();
        if let Some(dirs) = networked.harness.as_mut() {
            dirs.network = Some(HarnessNetwork {
                relay: 8787,
                handoff: PathBuf::from("/run/handoff"),
                tools: vec![(PathBuf::from("/bin/sh"), String::from("bin/sh"))],
                env: vec![(String::from("CLI_KEY"), String::from("surrogate"))],
            });
        }
        networked
    }

    #[test]
    fn only_a_networked_harness_cell_has_the_cli_filter_its_relay_and_its_tools() {
        let scratch = Scratch::new("network");
        let (skill, harness) = cells(&scratch);
        let networked = networked(&harness);
        assert_eq!(cell_filter(&networked), Filter::Cli);
        assert!(harness_network(&skill).is_none());
        assert!(harness_network(&harness).is_none());
        assert_eq!(
            harness_network(&networked).map(|network| network.relay),
            Some(8787)
        );
        let spec = cell_spec(&networked, LogLevel::Error);
        assert_eq!(spec.relay, Some(8787));
        assert_eq!(
            spec.tools,
            [Tool {
                source: PathBuf::from("/bin/sh"),
                name: String::from("bin/sh"),
            }]
        );
        assert!(
            spec.env
                .contains(&(String::from("CLI_KEY"), String::from("surrogate")))
        );
        let plain = cell_spec(&harness, LogLevel::Error);
        assert_eq!(plain.relay, None);
        assert!(plain.tools.is_empty());
        assert!(!plain.env.iter().any(|(key, _)| key == "CLI_KEY"));
    }

    #[test]
    fn a_harness_cell_starts_in_its_work_tree_and_every_other_cell_at_the_root() {
        let scratch = Scratch::new("workdir");
        let (skill, harness) = cells(&scratch);
        assert_eq!(cell_workdir(&harness), Some("/work"));
        assert_eq!(cell_workdir(&skill), None);
        assert_eq!(cell_workdir(&CellConfig::skill("agent").unwrap()), None);
    }

    #[test]
    fn only_a_harness_cell_runs_under_the_work_tree_filter() {
        let scratch = Scratch::new("profile");
        let (skill, harness) = cells(&scratch);
        assert_eq!(cell_filter(&harness), Filter::WorkTree);
        assert_eq!(cell_filter(&skill), Filter::Program);
        assert_eq!(
            cell_filter(&CellConfig::skill("agent").unwrap()),
            Filter::Program
        );
    }

    #[test]
    fn a_harness_cell_writes_only_its_work_tree_and_scratch() {
        let scratch = Scratch::new("plan");
        let (skill, harness) = cells(&scratch);
        let writable = |config: &CellConfig| -> Vec<(String, PathBuf)> {
            cell_exposures(config)
                .into_iter()
                .filter(|exposure| exposure.writable)
                .map(|exposure| (exposure.name, exposure.source))
                .collect()
        };
        assert_eq!(
            writable(&harness),
            [
                (String::from("work"), scratch.0.join("work")),
                (String::from("scratch"), scratch.0.join("scratch"))
            ]
        );
        assert!(writable(&skill).is_empty(), "no other cell writes anywhere");
        let names = |config: &CellConfig| -> Vec<String> {
            cell_exposures(config)
                .into_iter()
                .map(|exposure| exposure.name)
                .collect()
        };
        assert_eq!(names(&skill), ["skill"]);
        assert_eq!(names(&harness), ["skill", "work", "scratch"]);
        assert!(cell_exposures(&CellConfig::skill("agent").unwrap()).is_empty());
        let spec = cell_spec(&harness, LogLevel::Error);
        assert_eq!(spec.workdir.as_deref(), Some("/work"));
        assert_eq!(spec.filter, Filter::WorkTree);
        assert_eq!(spec.endpoint, None, "a harness cell has no socket");
    }

    #[test]
    fn a_harness_cell_lives_in_scratch_and_runs_with_its_arguments() {
        let scratch = Scratch::new("env");
        let (_, harness) = cells(&scratch);
        let (argv, env) = program_env(&harness, LogLevel::Error);
        assert_eq!(argv, ["/agent", "-p", "Go."]);
        let keys: Vec<&str> = env.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            ["HATCHLING_CELL_ID", "HATCHLING_LOG_LEVEL", "HOME", "TMPDIR"]
        );
        assert!(
            env.iter()
                .filter(|(key, _)| key == "HOME" || key == "TMPDIR")
                .all(|(_, value)| value == "/scratch")
        );
    }

    /// the vault and its key are never where the agent cell can
    /// see them; a directory beside the socket's is not inside it.
    #[test]
    fn the_vault_is_refused_inside_the_directory_the_cell_mounts() {
        let mut cell = CellConfig::skill("cell-a").unwrap();
        cell.socket_dir = PathBuf::from("/run/hatchling");
        let vault = |root: &str, key: &str| crate::VaultConfig {
            root: PathBuf::from(root),
            key_file: PathBuf::from(key),
            grants: Vec::new(),
        };
        let refused = |result: Result<(), Error>| match result {
            Err(Error::InvalidConfig { key, reason }) if reason.contains("/run/hatchling") => key,
            other => unreachable!("not refused: {other:?}"),
        };
        assert_eq!(
            refused(cell.refuse_vault_in_view(&vault("/run/hatchling/vault", "/srv/key/key"))),
            "HATCHLING_VAULT_DIR"
        );
        assert_eq!(
            refused(cell.refuse_vault_in_view(&vault("/srv/vault", "/run/hatchling/key"))),
            "HATCHLING_VAULT_KEY_FILE"
        );
        cell.refuse_vault_in_view(&vault("/srv/vault", "/srv/vault.key/key"))
            .unwrap();
        cell.refuse_vault_in_view(&vault("/run/hatchling-vault", "/run/hatchling-key/key"))
            .unwrap();
    }
}
