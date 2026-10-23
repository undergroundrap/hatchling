//! Broker configuration, parsed once at startup.
//!
//! Invariant: a `Config` value is fully validated. Everything downstream
//! may assume validity without re-checking. Untrusted input (environment
//! variables) becomes validated types here, at the edge, exactly once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hatchling_core::{
    CapabilityName, LogLevel, PeerUid, ScheduleGrant, SecretName, SocketPath, VaultAction,
};

use crate::Error;
use crate::cell::CellIds;
use crate::environment as env;

/// Default socket mode: owner-only. The fail-closed default — the
/// operator opens it deliberately for the production uid layout.
const DEFAULT_SOCKET_MODE: u32 = 0o600;

/// Validated broker configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Absolute path of the Unix socket the broker listens on.
    pub socket: SocketPath,
    /// The only uid the broker will talk to, from the kernel's point of
    /// view. v0.1 policy is exact equality; the peer is authorized
    /// against this value via `SO_PEERCRED` before any of its bytes are
    /// parsed. There is no other identity signal.
    pub agent_uid: PeerUid,
    /// Permission bits applied to the socket after bind, as a Unix mode
    /// (e.g. `0o600`). Validated to be within `0o000`–`0o777`.
    pub socket_mode: u32,
    /// Most connections the broker serves at once ; past it, new
    /// connections are closed silently. `HATCHLING_BROKER_MAX_CONNECTIONS`,
    /// 1–[`MAX_MAX_CONNECTIONS`], default [`DEFAULT_MAX_CONNECTIONS`].
    pub max_connections: usize,
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Vault mediation: where secrets live and what this agent may do.
    pub vault: VaultConfig,
    /// Egress mediation: where the agent may send network requests.
    pub egress: EgressConfig,
    /// Channel mediation: which channel endpoints the broker owns.
    /// The local channel (`local`) is always available; additional ids
    /// name integrations added by later WOs.
    pub channels: Vec<String>,
    /// Channel accounts: which vault secret each account of a
    /// credentialed channel authenticates with. The broker alone loads
    /// them; no vault grant may name one, so no cell op can reach it.
    pub channel_accounts: Vec<ChannelAccountSpec>,
    /// The local channel's ingress socket: where the local user
    /// injects messages and reads the agent's replies. `None` leaves the
    /// local channel without ingress. Always outside the broker socket's
    /// directory, which the cell mounts.
    pub channel_ingress: Option<SocketPath>,
    /// Skill mediation: which signed skills are loaded and what they may do.
    pub skills: SkillConfig,
    /// Memory mediation: where memory items live and what this agent may do.
    pub memory: MemoryConfig,
    /// Goals and tracking mediation: where goals and the tracking log
    /// live, and what this agent may do with them.
    pub goals: GoalsConfig,
    /// Scheduled work: where the sealed schedule store lives.
    pub scheduler: SchedulerConfig,
    /// The artifact pipeline: where builds are kept, where
    /// workspace deliveries go, and the share-review policy.
    pub artifacts: ArtifactConfig,
    /// The space store, and for a space task's run session, the
    /// one space it is bound to.
    pub spaces: SpaceConfig,
    /// The web, for the agent (WO-028).
    pub web: WebConfig,
}

/// The artifact pipeline's configuration.
///
/// The build store is a sealed vault of its own at `root`, under its own
/// key, apart from the credential vault. `workspace` is
/// the operator's directory that workspace deliveries write into. Every
/// delivery is judged by the share-review gate against `policy`; with no
/// policy configured, the gate cannot judge, and every delivery is
/// refused (fail-closed).
#[derive(Debug, Clone)]
pub struct ArtifactConfig {
    /// Absolute path of the build store root. Empty means artifacts are
    /// off: every artifact op is denied.
    pub root: PathBuf,
    /// Absolute path of the workspace deliveries write into, if any.
    pub workspace: Option<PathBuf>,
    /// The share-review policy, if one is configured.
    pub policy: Option<hatchling_artifact::ReviewPolicy>,
    /// The build kinds the broker builds: one profile each,
    /// which the inspector checks every completed build against. A kind
    /// with no profile is refused.
    pub kinds: hatchling_artifact::Kinds,
}

impl ArtifactConfig {
    /// Artifacts off.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            root: PathBuf::new(),
            workspace: None,
            policy: None,
            kinds: hatchling_artifact::Kinds::standard(),
        }
    }

    /// Whether a build store is configured.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.root.as_os_str().is_empty()
    }
}

/// The space store's configuration.
///
/// The store is a sealed vault of its own at `root`, under its own key,
/// apart from the credential vault and the other stores.
/// `bound` is set only by [`crate::scoped_config`], for a space task's run
/// session: that session's principal is the task run, and it may use that
/// one space and no other.
#[derive(Debug, Clone)]
pub struct SpaceConfig {
    /// Absolute path of the space store root. Empty means spaces are off:
    /// every space op is denied.
    pub root: PathBuf,
    /// For a space task's run session only: the space it is a task of.
    pub bound: Option<String>,
    /// The device endpoint's socket, if devices may pair and
    /// sync. Never set for a run session.
    pub device_socket: Option<SocketPath>,
}

impl SpaceConfig {
    /// Spaces off.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            root: PathBuf::new(),
            bound: None,
            device_socket: None,
        }
    }

    /// Whether a space store is configured.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.root.as_os_str().is_empty()
    }
}

/// Broker-side scheduler configuration.
///
/// The schedule store is a sealed vault of its own at `root`, under its
/// own key. Config parsing keeps it apart from the credential
/// vault, so no vault grant can ever name one of its entries.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Absolute path of the schedule store root. Empty means scheduling
    /// is off: every schedule op is denied.
    pub root: PathBuf,
    /// The model endpoint scheduled runs use
    /// (`HATCHLING_SCHEDULER_MODEL_ENDPOINT`), handed to each run cell as
    /// `HATCHLING_MODEL_ENDPOINT` (-2). The run reaches it only
    /// through an egress grant the schedule holds.
    pub model_endpoint: Option<String>,
    /// The model name scheduled runs use (`HATCHLING_SCHEDULER_MODEL_NAME`).
    pub model_name: Option<String>,
    /// Thinking mode supplied to scheduled and child runs.
    pub model_reasoning: hatchling_core::ModelReasoning,
    /// The agent binary run cells execute (`HATCHLING_AGENT_BIN`, the
    /// same binary as the agent cell's). Without it the broker starts no
    /// dispatcher: there is nothing to run.
    pub agent_bin: Option<PathBuf>,
    /// For a scheduled or hook run's own session only: what the
    /// broker knows about the run, so the session can take its one
    /// follow-up request. Never parsed from the environment: only
    /// `scoped_config` sets it, and the broker's own session has none.
    pub followup: Option<FollowupScope>,
    /// How deep this session sits as a subagent: 0 for any
    /// session but a child's. Never parsed from the environment: only a
    /// child's scoped configuration sets it, and a session at
    /// [`hatchling_core::subagent::MAX_DEPTH`] spawns nothing.
    pub subagent_depth: u8,
}

/// A run session's follow-up scope: the one scheduler surface a run's
/// session has. Every field is the broker's, set when it
/// started the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupScope {
    /// The schedule store root the follow-up is recorded in.
    pub root: PathBuf,
    /// The run's token: its one follow-up is claimed under it.
    pub run: String,
    /// The run's grants: a follow-up may hold only these.
    pub grants: Vec<ScheduleGrant>,
    /// The run's output channel, which a follow-up's output goes to.
    pub channel: String,
    /// The run's depth in its follow-up chain.
    pub depth: u32,
}

impl SchedulerConfig {
    /// Scheduling off.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            root: PathBuf::new(),
            model_endpoint: None,
            model_name: None,
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            agent_bin: None,
            followup: None,
            subagent_depth: 0,
        }
    }

    /// Where scheduled runs make their per-run directories
    /// (-2): the store root's sibling `<root>.runs`, apart from
    /// the store itself, the vault, and the broker socket directory.
    #[must_use]
    pub fn run_root(&self) -> PathBuf {
        self.root.with_extension("runs")
    }

    /// Where subagent activity is recorded: the store root's
    /// sibling `<root>.activity`, one JSON event per line.
    #[must_use]
    pub fn activity_log(&self) -> PathBuf {
        self.root.with_extension("activity")
    }

    /// The same config, recording `agent_bin` when one is set.
    #[must_use]
    pub fn with_agent_bin(mut self, agent_bin: Option<String>) -> Self {
        self.agent_bin = agent_bin
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from);
        self
    }

    /// Whether a store is configured.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.root.as_os_str().is_empty()
    }
}

/// Broker-side vault mediation configuration.
///
/// The broker is the only writer and reader of the credential store. The
/// agent never sees these paths; it names secrets, and the broker maps
/// names to files after the grant check.
#[derive(Debug, Clone)]
pub struct VaultConfig {
    /// Absolute path of the vault root directory. Created `0700` on
    /// first open; every session re-validates mode and owner, so
    /// tampering between sessions fails closed.
    pub root: PathBuf,
    /// Absolute path of the master-key file. `0600`, created with
    /// `OsRng` bytes on first broker start; the broker seals every
    /// secret under this key — there is no unsealed mode. Defaults to
    /// [`VaultConfig::default_key_file`]: deliberately *not* inside
    /// the vault dir, so a backup of the store cannot silently capture
    /// the key with it.
    pub key_file: PathBuf,
    /// Explicit (action, name) grants. Empty means deny everything: the
    /// broker answers [`Denied`](hatchling_core::VaultResult::Denied)
    /// for any op it holds no grant for, before touching the vault.
    pub grants: Vec<VaultGrant>,
}

impl VaultConfig {
    /// The default master-key file for a vault root: `<root>.key/key`.
    ///
    /// A dedicated `0700` key directory sibling to the vault root,
    /// holding the `0600` key file. A bare `<root>.key` sibling *file*
    /// cannot be the default: the key file's parent must be
    /// owner-only, and the vault root's parent generally is not
    /// (`/tmp`, `/var/lib`, …). The dedicated directory is created
    /// `0700` on first provisioning, so the default works anywhere an
    /// absolute vault root does. The key stays outside the secret
    /// store itself, so a backup of the store cannot silently capture
    /// the key with it.
    #[must_use]
    pub fn default_key_file(vault_root: &Path) -> PathBuf {
        vault_root.with_extension("key").join("key")
    }
}

/// One explicit grant: the agent may perform `action` on `name`.
///
/// Grants are deny-by-default and name-exact: there are no wildcards, no
/// prefixes, and no list grant. A grant for `fetch:app-token` says
/// nothing about `store:app-token` or any other name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultGrant {
    /// The permitted action.
    pub action: VaultAction,
    /// The permitted secret name.
    pub name: SecretName,
}

/// One account of a credentialed channel: the channel, the
/// operator's label for the account, and the vault secret it
/// authenticates with.
///
/// The secret is custody-only: the broker loads it when it builds a
/// session's channels, and config parsing refuses any vault grant that
/// names it, so the cell can neither fetch nor overwrite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAccountSpec {
    /// The channel id.
    pub channel: String,
    /// The account's label: 1–32 of `a-z`, `0-9`, `-`.
    pub label: String,
    /// The vault secret holding the account's credential.
    pub secret: SecretName,
}

/// Broker-side memory mediation configuration.
///
/// The broker is the only writer and reader of the memory store. The
/// agent never sees these paths; it names scopes and ids, and the
/// broker binds them through the peer-asserted principal after the
/// grant check.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Absolute path of the memory store root directory. Created `0700`
    /// on first open; every op re-validates, so tampering between ops
    /// fails closed.
    pub root: PathBuf,
    /// Explicit (scope, action) grants. Empty means deny everything:
    /// the broker answers [`Denied`](hatchling_core::MemoryResult::Denied)
    /// for any op it holds no grant for, before touching the store.
    pub grants: Vec<MemoryGrant>,
}

/// One memory grant: a (scope, action) pair the agent may perform.
///
/// Scopes are `user`, `project`, or `session`. Context ops
/// (`context-append`, `context-read`) are always `session` scope.
#[derive(Debug, Clone)]
pub struct MemoryGrant {
    /// The permitted scope: `user`, `project`, or `session`.
    pub scope: String,
    /// The permitted action.
    pub action: MemoryAction,
}

/// Broker-side goals and tracking mediation configuration.
///
/// The broker is the only writer and reader of the goals store, like
/// the memory store it lives beside. The agent names goals and sources;
/// the broker binds every op to the peer-asserted principal after the
/// grant check.
#[derive(Debug, Clone)]
pub struct GoalsConfig {
    /// Absolute path of the goals store root directory. Created `0700`
    /// on first write; every op re-validates, so tampering between ops
    /// fails closed.
    pub root: PathBuf,
    /// Explicit action grants. Empty means deny everything: the broker
    /// answers `Denied` for any op it holds no grant for, before
    /// touching the store. Memory queries are gated by the memory
    /// grants (`user:recall`), not by these.
    pub grants: Vec<GoalsAction>,
}

impl GoalsConfig {
    /// A disabled goals store: no root, no grants, every op denied.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            root: PathBuf::new(),
            grants: Vec::new(),
        }
    }
}

/// The action a goals op or query performs: the unit of grant matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalsAction {
    /// Record a new goal.
    GoalCreate,
    /// Change an open goal's title or detail.
    GoalUpdate,
    /// Close an open goal.
    GoalClose,
    /// Append to the tracking log.
    TrackingAppend,
    /// Query goals.
    QueryGoals,
    /// Query the tracking log.
    QueryTracking,
}

impl GoalsAction {
    /// The action a configuration name names, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::GoalCreate,
            Self::GoalUpdate,
            Self::GoalClose,
            Self::TrackingAppend,
            Self::QueryGoals,
            Self::QueryTracking,
        ]
        .into_iter()
        .find(|action| action.name() == name)
    }

    /// The action's configuration name (`goal-create`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::GoalCreate => "goal-create",
            Self::GoalUpdate => "goal-update",
            Self::GoalClose => "goal-close",
            Self::TrackingAppend => "tracking-append",
            Self::QueryGoals => "query-goals",
            Self::QueryTracking => "query-tracking",
        }
    }
}

/// The action a memory op performs: the unit of grant matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAction {
    /// Store (create or replace) an item.
    Write,
    /// Recall one item or by tag.
    Recall,
    /// Remove one item.
    Remove,
    /// Append to the session context log.
    ContextAppend,
    /// Read the session context log.
    ContextRead,
}

impl MemoryAction {
    /// The action a configuration name names, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Write,
            Self::Recall,
            Self::Remove,
            Self::ContextAppend,
            Self::ContextRead,
        ]
        .into_iter()
        .find(|action| action.name() == name)
    }

    /// The action's configuration name (`write`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Recall => "recall",
            Self::Remove => "remove",
            Self::ContextAppend => "context-append",
            Self::ContextRead => "context-read",
        }
    }
}

/// Broker-side egress mediation configuration.
///
/// The broker is the only component that touches the network on the
/// agent's behalf. The agent never sees these values; it names
/// destinations, and the broker validates them against the allowlist
/// after the grant check.
#[derive(Debug, Clone)]
pub struct EgressConfig {
    /// The `host:port` allowlist, resolved once (see [`EgressAllowlist`]).
    /// Empty means deny everything: the broker answers
    /// [`Denied`](hatchling_core::EgressResult::Denied) for any egress
    /// op.
    pub allowlist: EgressAllowlist,
    /// Explicit destination grants. Empty means deny everything.
    pub grants: Vec<String>,
}

/// The web, for the agent (WO-028): whether it may ask to read pages, and
/// which sites the operator refuses.
///
/// A site the agent asks for is put to the owner, who allows it once or
/// for the session, or says no; the operator's word bounds both. Off
/// unless the operator turns it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfig {
    /// Whether the agent may ask to read pages (`HATCHLING_WEB=sites`).
    pub enabled: bool,
    /// Hosts the operator refuses (`HATCHLING_WEB_REFUSE`): a site on one,
    /// or on a name under it, is never put to the owner.
    pub refused: Vec<String>,
    /// Whether the session's asks wait for the owner to decide: a person is
    /// there (the agent's own session, a chat). A scheduled run's don't: it
    /// asks, and goes on without the page.
    pub attended: bool,
    /// Whether the agent may search, through Brave Search, with the key the
    /// egress authority reads (`HATCHLING_SEARCH=brave`).
    pub search: bool,
    /// What the broker logs of each search (`HATCHLING_SEARCH_LOG`).
    pub search_log: SearchLog,
    /// Where the broker keeps the sites the owner always allows
    /// (`HATCHLING_WEB_GRANTS_DIR`, a sealed store); empty, and *Allow
    /// always* isn't offered.
    pub grants: PathBuf,
    /// Where the broker keeps the browser's signed-in profiles
    /// (`HATCHLING_BROWSER_PROFILES_DIR`, a sealed store, WO-030); empty,
    /// and *Keep me signed in* isn't offered.
    pub profiles: PathBuf,
    /// Where the broker keeps the sign-ins it fills for signed-in profiles
    /// (`HATCHLING_BROWSER_CREDENTIALS_DIR`, a sealed store, WO-030);
    /// empty, and none is kept.
    pub credentials: PathBuf,
}

/// What the broker logs of each search (WO-028).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLog {
    /// Its session and its length: the default.
    Length,
    /// Its text too: the owner's opt-in.
    Queries,
}

impl WebConfig {
    /// The web off: nothing is fetched, and nothing is asked.
    #[must_use]
    pub fn off() -> Self {
        Self {
            enabled: false,
            refused: Vec::new(),
            attended: false,
            search: false,
            search_log: SearchLog::Length,
            grants: PathBuf::new(),
            profiles: PathBuf::new(),
            credentials: PathBuf::new(),
        }
    }

    /// Whether the operator refuses `host`: it is, or is under, a refused
    /// host.
    #[must_use]
    pub fn refuses(&self, host: &str) -> bool {
        self.refused.iter().any(|refused| {
            host == refused
                || host
                    .strip_suffix(refused.as_str())
                    .is_some_and(|under| under.ends_with('.'))
        })
    }
}

/// Parses the web settings: `HATCHLING_WEB` (`off`, the default, or
/// `sites`), `HATCHLING_WEB_REFUSE` (comma-separated hosts, each a name of
/// ASCII labels), `HATCHLING_SEARCH` (`off`, the default, or `brave`) and
/// `HATCHLING_SEARCH_LOG` (`length`, the default, or `queries`).
fn parse_web(get: &impl Fn(&str) -> Option<String>) -> Result<WebConfig, Error> {
    let web = get(env::HATCHLING_WEB.name);
    let refuse = get(env::HATCHLING_WEB_REFUSE.name);
    let enabled = match web.as_deref().map(str::trim) {
        None | Some("" | "off") => false,
        Some("sites") => true,
        Some(other) => {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_WEB",
                reason: format!("must be off or sites, not {other:?}"),
            });
        }
    };
    let mut refused = Vec::new();
    for host in refuse
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
    {
        if host.is_empty() {
            continue;
        }
        let parsed = hatchling_egress::web::WebUrl::parse(&format!("https://{host}/"))
            .ok()
            .filter(|url| url.origin().host() == host && url.origin().port() == 443);
        if parsed.is_none() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_WEB_REFUSE",
                reason: format!("{host:?} is not a host name of lowercase ASCII labels"),
            });
        }
        refused.push(host.to_owned());
    }
    let search = match get(env::HATCHLING_SEARCH.name).as_deref().map(str::trim) {
        None | Some("" | "off") => false,
        Some("brave") => true,
        Some(other) => {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_SEARCH",
                reason: format!("must be off or brave, not {other:?}"),
            });
        }
    };
    let search_log = match get(env::HATCHLING_SEARCH_LOG.name)
        .as_deref()
        .map(str::trim)
    {
        None | Some("" | "length") => SearchLog::Length,
        Some("queries") => SearchLog::Queries,
        Some(other) => {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_SEARCH_LOG",
                reason: format!("must be length or queries, not {other:?}"),
            });
        }
    };
    let grants = store_dir(
        "HATCHLING_WEB_GRANTS_DIR",
        get(env::HATCHLING_WEB_GRANTS_DIR.name).as_deref(),
    )?;
    let profiles = store_dir(
        "HATCHLING_BROWSER_PROFILES_DIR",
        get(env::HATCHLING_BROWSER_PROFILES_DIR.name).as_deref(),
    )?;
    let credentials = store_dir(
        "HATCHLING_BROWSER_CREDENTIALS_DIR",
        get(env::HATCHLING_BROWSER_CREDENTIALS_DIR.name).as_deref(),
    )?;
    Ok(WebConfig {
        enabled,
        refused,
        attended: true,
        search,
        search_log,
        grants,
        profiles,
        credentials,
    })
}

/// The sealed store `raw` names for the setting `key`: none when it's unset
/// or empty.
///
/// # Errors
///
/// [`Error::InvalidConfig`]: a path that isn't absolute.
fn store_dir(key: &'static str, raw: Option<&str>) -> Result<PathBuf, Error> {
    match raw.map(str::trim) {
        None | Some("") => Ok(PathBuf::new()),
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(Error::InvalidConfig {
                    key,
                    reason: format!("must be an absolute path: {}", path.display()),
                })
            }
        }
    }
}

/// The egress allowlist: the operator's comma-separated `host:port` list,
/// resolved once.
///
/// The first [`resolve`](Self::resolve) parses it, resolves each
/// destination and pins its addresses; every later call, on this value or
/// any copy of it, returns that same [`Allowlist`]. Each session's egress
/// proxy is built over it, so no session resolves a destination again:
/// what a destination's DNS says later doesn't change where its traffic
/// goes. A resolution that fails pins nothing, and the next tries again.
#[derive(Clone, Default)]
pub struct EgressAllowlist {
    raw: String,
    resolved: Arc<Mutex<Option<Arc<hatchling_egress::Allowlist>>>>,
}

impl EgressAllowlist {
    /// The allowlist the operator wrote, not resolved yet.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            resolved: Arc::default(),
        }
    }

    /// The allowlist as the operator wrote it.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The allowlist, parsed with its destinations' addresses pinned: the
    /// first time, resolved now; after that, the same one.
    ///
    /// # Errors
    ///
    /// Returns [`hatchling_egress::Error::InvalidAllowlist`] if an entry
    /// is malformed or a destination doesn't resolve.
    pub fn resolve(&self) -> Result<Arc<hatchling_egress::Allowlist>, hatchling_egress::Error> {
        // Held while resolving: two sessions starting at once resolve it
        // once between them.
        let mut resolved = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
        let allowlist = if let Some(pinned) = resolved.as_ref() {
            Arc::clone(pinned)
        } else {
            let allowlist = Arc::new(hatchling_egress::Allowlist::parse(&self.raw)?);
            *resolved = Some(Arc::clone(&allowlist));
            allowlist
        };
        drop(resolved);
        Ok(allowlist)
    }
}

impl std::fmt::Debug for EgressAllowlist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressAllowlist")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

/// Validated skill configuration.
///
/// Skills are signed manifests with declared capabilities, loaded from
/// disk at startup. The broker verifies admission before registering.
#[derive(Debug, Clone)]
pub struct SkillConfig {
    /// Directory containing skill manifests. Each subdirectory should
    /// have a `manifest.json` (signed) and the skill code.
    pub skills_dir: PathBuf,
    /// Absolute path of the trust-roots file: the keys skill signatures
    /// must verify under (see [`crate::trust_roots`]). Read at startup
    /// and re-read on change, so roots rotate without a restart.
    pub trust_roots_file: PathBuf,
    /// Explicit capability grants per skill. Maps skill name to allowed
    /// capabilities, each validated at startup: a malformed grant stops
    /// the broker rather than loading as a grant that matches nothing.
    pub grants: HashMap<String, HashSet<CapabilityName>>,
}

impl SkillConfig {
    /// No skills: no skills dir, no trust root, no grants. The broker
    /// loads nothing and every skill op names an unknown skill.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            skills_dir: PathBuf::new(),
            trust_roots_file: PathBuf::new(),
            grants: HashMap::new(),
        }
    }

    /// Whether a skills dir is configured at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.skills_dir.as_os_str().is_empty()
    }
}

impl Config {
    /// Reads and validates configuration from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if any required value is missing or invalid.
    pub fn from_env() -> Result<Self, Error> {
        Self::parse(|key| match std::env::var(key) {
            Ok(value) => Some(value),
            // A present malformed OS value is invalid, not an absent setting.
            Err(std::env::VarError::NotUnicode(_))
                if key == env::HATCHLING_SCHEDULER_MODEL_REASONING.name =>
            {
                Some(String::new())
            }
            Err(_) => None,
        })
    }

    /// Parses configuration using a key lookup function.
    ///
    /// Split out so tests can inject values without touching process
    /// environment (tests run in parallel; the process env is shared).
    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let socket = get(env::HATCHLING_BROKER_SOCKET.name)
            .ok_or(Error::MissingConfig("HATCHLING_BROKER_SOCKET"))?
            .parse::<SocketPath>()?;
        let agent_uid = parse_agent_uid(&get, || {
            use hatchling_isolation::IsolationBackend as _;
            Ok(hatchling_isolation::platform()
                .subordinate_identity()?
                .principal)
        })?;
        let socket_mode = get(env::HATCHLING_BROKER_SOCKET_MODE.name)
            .map_or(Ok(DEFAULT_SOCKET_MODE), |raw| parse_socket_mode(&raw))?;
        let log_level = get(env::HATCHLING_LOG_LEVEL.name)
            .map_or(Ok(LogLevel::Info), |raw| raw.parse::<LogLevel>())?;
        let vault_dir = get(env::HATCHLING_VAULT_DIR.name)
            .ok_or(Error::MissingConfig("HATCHLING_VAULT_DIR"))?;
        let vault_dir = PathBuf::from(vault_dir);
        if !vault_dir.is_absolute() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_VAULT_DIR",
                reason: format!(
                    "vault dir must be an absolute path: {}",
                    vault_dir.display()
                ),
            });
        }
        let vault_grants = parse_grants(get(env::HATCHLING_VAULT_GRANTS.name))?;
        // The master-key file defaults to a dedicated 0700 directory
        // sibling to the vault root (see
        // [`VaultConfig::default_key_file`]): never *inside* the vault
        // dir, so a backup of the store cannot silently capture the key
        // with it — and never a bare file in the vault root's parent,
        // which is generally not owner-only. An explicit path must be
        // absolute for the same reason the vault dir must be — a
        // relative key path would resolve against the broker's cwd.
        let key_file = match get(env::HATCHLING_VAULT_KEY_FILE.name) {
            Some(raw) => {
                let path = PathBuf::from(raw);
                if !path.is_absolute() {
                    return Err(Error::InvalidConfig {
                        key: "HATCHLING_VAULT_KEY_FILE",
                        reason: format!(
                            "vault key file must be an absolute path: {}",
                            path.display()
                        ),
                    });
                }
                path
            }
            None => VaultConfig::default_key_file(&vault_dir),
        };
        let egress_allowlist = get(env::HATCHLING_EGRESS_ALLOWLIST.name).unwrap_or_default();
        let egress_grants = parse_egress_grants(get(env::HATCHLING_EGRESS_GRANTS.name))?;
        // The broker reads each egress destination's credential from the
        // vault: no grant may name one, and no two may share one.
        refuse_egress_secret_conflicts(&vault_grants, &egress_grants, &egress_allowlist)?;
        // Channels: comma-separated ids. The local channel is always
        // available; an empty list means only local. Unknown ids fail
        // at registry construction, not here — the error needs the
        // registry's knowledge of what exists.
        let channels = parse_channels(get(env::HATCHLING_CHANNELS.name))?;
        // Channel accounts: `channel:label=secret-name`, comma-separated.
        // Checked against the channel list and the vault grants here, so
        // a secret a cell could reach fails the broker at startup.
        let channel_accounts = parse_channel_accounts(
            get(env::HATCHLING_CHANNEL_ACCOUNTS.name).as_deref(),
            &channels,
            &vault_grants,
        )?;
        // Channel ingress: an absolute socket path, outside the directory
        // the cell mounts, for the local channel.
        let channel_ingress = parse_channel_ingress(
            get(env::HATCHLING_CHANNEL_INGRESS.name).as_deref(),
            &socket,
            &channels,
        )?;
        // Skills: directory of signed skill manifests. Empty means no
        // skills loaded. Trust root is required if a skills dir is set.
        // The inline key moved to a file so it can rotate: refuse the old
        // variable by name rather than silently ignore a key someone set.
        if get(env::HATCHLING_SKILL_TRUST_ROOT.name).is_some() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_SKILL_TRUST_ROOT",
                reason: String::from(
                    "replaced by HATCHLING_SKILL_TRUST_ROOTS, the absolute path of a trust-roots file",
                ),
            });
        }
        let skills = parse_skills(
            get(env::HATCHLING_SKILLS_DIR.name),
            get(env::HATCHLING_SKILL_TRUST_ROOTS.name),
            get(env::HATCHLING_SKILL_GRANTS.name),
        )?;
        // The stores: memory, goals, schedules, artifacts, and
        // spaces ; each a root directory (empty means off).
        let (memory, goals, scheduler, artifacts, spaces) =
            parse_stores(&get, &vault_dir, &socket)?;
        Ok(Self {
            socket,
            agent_uid,
            socket_mode,
            max_connections: parse_max_connections(
                get(env::HATCHLING_BROKER_MAX_CONNECTIONS.name).as_deref(),
            )?,
            log_level,
            vault: VaultConfig {
                root: vault_dir,
                key_file,
                grants: vault_grants,
            },
            egress: EgressConfig {
                allowlist: EgressAllowlist::new(egress_allowlist),
                grants: egress_grants,
            },
            channels,
            channel_accounts,
            channel_ingress,
            skills,
            memory,
            goals,
            scheduler,
            artifacts,
            spaces,
            web: parse_web(&get)?,
        })
    }
}

/// Parses the memory configuration: store root and grants.
///
/// Missing dir means memory is disabled — the store is never opened
/// and every op is denied. Grants are comma-separated `scope:action`
/// pairs (`user:write`, `session:context-append`, …); an unknown scope
/// or action fails closed.
fn parse_memory(dir: Option<String>, grants_raw: Option<String>) -> Result<MemoryConfig, Error> {
    let Some(dir) = dir else {
        return Ok(MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        });
    };
    if dir.trim().is_empty() {
        return Ok(MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        });
    }
    let root = PathBuf::from(dir);
    if !root.is_absolute() {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_MEMORY_DIR",
            reason: format!("memory dir must be an absolute path: {}", root.display()),
        });
    }
    let mut grants = Vec::new();
    if let Some(raw) = grants_raw {
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (scope, action) = entry.split_once(':').ok_or_else(|| Error::InvalidConfig {
                key: "HATCHLING_MEMORY_GRANTS",
                reason: format!("expected 'scope:action', got {entry:?}"),
            })?;
            let scope = scope.trim().to_owned();
            if scope != "user" && scope != "project" && scope != "session" {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_MEMORY_GRANTS",
                    reason: format!("unknown memory scope {scope:?}"),
                });
            }
            let action = match action.trim() {
                "write" => MemoryAction::Write,
                "recall" => MemoryAction::Recall,
                "remove" => MemoryAction::Remove,
                "context-append" => MemoryAction::ContextAppend,
                "context-read" => MemoryAction::ContextRead,
                other => {
                    return Err(Error::InvalidConfig {
                        key: "HATCHLING_MEMORY_GRANTS",
                        reason: format!("unknown memory action {other:?}"),
                    });
                }
            };
            grants.push(MemoryGrant { scope, action });
        }
    }
    Ok(MemoryConfig { root, grants })
}

/// Parses the goals configuration: store root and grants.
///
/// Missing dir means the goals store is disabled — it is never opened
/// and every op is denied. Grants are comma-separated actions
/// (`goal-create`, `goal-update`, `goal-close`, `tracking-append`,
/// `query-goals`, `query-tracking`); an unknown action fails closed.
/// Parses the store directories: memory and goals with their grants
/// (`HATCHLING_MEMORY_*`, `HATCHLING_GOALS_*`; grants are comma-separated,
/// e.g. `user:write` and `goal-create`), the schedule store, the
/// artifact pipeline's, and the space store.
fn parse_stores(
    get: &impl Fn(&str) -> Option<String>,
    vault_root: &Path,
    socket: &SocketPath,
) -> Result<
    (
        MemoryConfig,
        GoalsConfig,
        SchedulerConfig,
        ArtifactConfig,
        SpaceConfig,
    ),
    Error,
> {
    let socket_dir = socket.as_path().parent();
    let mut scheduler = parse_scheduler(
        get(env::HATCHLING_SCHEDULER_DIR.name).as_deref(),
        (
            get(env::HATCHLING_SCHEDULER_MODEL_ENDPOINT.name),
            get(env::HATCHLING_SCHEDULER_MODEL_NAME.name),
        ),
        vault_root,
        socket_dir,
    )?
    .with_agent_bin(get(env::HATCHLING_AGENT_BIN.name));
    scheduler.model_reasoning = match get(env::HATCHLING_SCHEDULER_MODEL_REASONING.name) {
        None => hatchling_core::ModelReasoning::ProviderDefault,
        Some(raw) => {
            hatchling_core::ModelReasoning::parse(&raw).ok_or_else(|| Error::InvalidConfig {
                key: "HATCHLING_SCHEDULER_MODEL_REASONING",
                reason: String::from("expected provider-default or disabled"),
            })?
        }
    };
    let artifacts = parse_artifacts(get, vault_root, socket_dir)?;
    let spaces = parse_spaces(
        (
            get(env::HATCHLING_SPACE_DIR.name).as_deref(),
            get(env::HATCHLING_DEVICE_SOCKET.name).as_deref(),
        ),
        vault_root,
        socket_dir,
        &[&scheduler.root, &artifacts.root],
    )?;
    Ok((
        parse_memory(
            get(env::HATCHLING_MEMORY_DIR.name),
            get(env::HATCHLING_MEMORY_GRANTS.name),
        )?,
        parse_goals(
            get(env::HATCHLING_GOALS_DIR.name),
            get(env::HATCHLING_GOALS_GRANTS.name),
        )?,
        scheduler,
        artifacts,
        spaces,
    ))
}

/// Parses the space store's configuration.
///
/// `HATCHLING_SPACE_DIR` unset or empty means spaces are off. Otherwise it
/// must be absolute, apart from the vault root and from every other
/// configured store in `stores`, and out of the broker socket directory
/// the cell mounts.
///
/// `HATCHLING_DEVICE_SOCKET` is the device endpoint: an absolute
/// socket path, out of that directory too, and only with a space store.
fn parse_spaces(
    (raw, device): (Option<&str>, Option<&str>),
    vault_root: &Path,
    socket_dir: Option<&Path>,
    stores: &[&Path],
) -> Result<SpaceConfig, Error> {
    let device_socket = parse_device_socket(device, socket_dir)?;
    let Some(raw) = raw.filter(|raw| !raw.trim().is_empty()) else {
        if device_socket.is_some() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_DEVICE_SOCKET",
                reason: String::from("devices sync spaces, and HATCHLING_SPACE_DIR is not set"),
            });
        }
        return Ok(SpaceConfig::disabled());
    };
    let root = PathBuf::from(raw.trim());
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_SPACE_DIR",
        reason,
    };
    if !root.is_absolute() {
        return Err(invalid(format!(
            "must be an absolute path: {}",
            root.display()
        )));
    }
    let other = std::iter::once(vault_root)
        .chain(stores.iter().copied())
        .filter(|other| !other.as_os_str().is_empty())
        .find(|other| overlaps(&root, other));
    if let Some(other) = other {
        return Err(invalid(format!(
            "{} overlaps another store at {}",
            root.display(),
            other.display()
        )));
    }
    if socket_dir.is_some_and(|socket_dir| root.starts_with(socket_dir)) {
        return Err(invalid(format!(
            "{} is inside the broker socket directory, which the cell mounts",
            root.display()
        )));
    }
    Ok(SpaceConfig {
        root,
        bound: None,
        device_socket,
    })
}

/// Parses the device endpoint's socket path: unset or empty
/// means none; otherwise absolute, and out of the directory the cell
/// mounts.
fn parse_device_socket(
    raw: Option<&str>,
    socket_dir: Option<&Path>,
) -> Result<Option<SocketPath>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_DEVICE_SOCKET",
        reason,
    };
    let socket: SocketPath = raw
        .parse()
        .map_err(|_| invalid(format!("must be an absolute socket path: {raw:?}")))?;
    if socket_dir.is_some_and(|dir| socket.as_path().starts_with(dir)) {
        return Err(invalid(format!(
            "{raw:?} is inside the broker socket directory, which the cell mounts"
        )));
    }
    Ok(Some(socket))
}

/// Whether either path contains the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Parses the artifact pipeline's configuration.
///
/// `HATCHLING_ARTIFACT_DIR` unset or empty means artifacts are off.
/// Otherwise it, and the optional `HATCHLING_ARTIFACT_WORKSPACE`, must be
/// absolute, apart from the vault root and from each other, and out of
/// the broker socket directory the cell mounts. The review policy is
/// `HATCHLING_ARTIFACT_MAX_BYTES` (a positive size) with the optional
/// `HATCHLING_ARTIFACT_DENY` (comma-separated substrings); with no size,
/// there is no policy, and the gate refuses every delivery.
fn parse_artifacts(
    get: &impl Fn(&str) -> Option<String>,
    vault_root: &Path,
    socket_dir: Option<&Path>,
) -> Result<ArtifactConfig, Error> {
    let Some(dir) = get(env::HATCHLING_ARTIFACT_DIR.name).filter(|dir| !dir.trim().is_empty())
    else {
        return Ok(ArtifactConfig::disabled());
    };
    let place = |key: &'static str, raw: &str| -> Result<PathBuf, Error> {
        let path = PathBuf::from(raw.trim());
        let invalid = |reason: String| Error::InvalidConfig { key, reason };
        if !path.is_absolute() {
            return Err(invalid(format!(
                "must be an absolute path: {}",
                path.display()
            )));
        }
        if overlaps(&path, vault_root) {
            return Err(invalid(format!(
                "{} overlaps the vault root {}",
                path.display(),
                vault_root.display()
            )));
        }
        if socket_dir.is_some_and(|socket_dir| path.starts_with(socket_dir)) {
            return Err(invalid(format!(
                "{} is inside the broker socket directory, which the cell mounts",
                path.display()
            )));
        }
        Ok(path)
    };
    let root = place("HATCHLING_ARTIFACT_DIR", &dir)?;
    let workspace =
        match get(env::HATCHLING_ARTIFACT_WORKSPACE.name).filter(|raw| !raw.trim().is_empty()) {
            Some(raw) => {
                let workspace = place("HATCHLING_ARTIFACT_WORKSPACE", &raw)?;
                if overlaps(&workspace, &root) {
                    return Err(Error::InvalidConfig {
                        key: "HATCHLING_ARTIFACT_WORKSPACE",
                        reason: format!(
                            "{} overlaps the build store {}",
                            workspace.display(),
                            root.display()
                        ),
                    });
                }
                Some(workspace)
            }
            None => None,
        };
    let deny: Vec<String> = get(env::HATCHLING_ARTIFACT_DENY.name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect();
    let policy = match get(env::HATCHLING_ARTIFACT_MAX_BYTES.name) {
        Some(raw) => {
            let max_bytes = raw
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|max| *max > 0)
                .ok_or_else(|| Error::InvalidConfig {
                    key: "HATCHLING_ARTIFACT_MAX_BYTES",
                    reason: format!("expected a positive size in bytes, got {raw:?}"),
                })?;
            Some(hatchling_artifact::ReviewPolicy { max_bytes, deny })
        }
        None if deny.is_empty() => None,
        None => {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_ARTIFACT_DENY",
                reason: String::from(
                    "a deny list is part of a review policy, which needs HATCHLING_ARTIFACT_MAX_BYTES",
                ),
            });
        }
    };
    Ok(ArtifactConfig {
        root,
        workspace,
        policy,
        kinds: hatchling_artifact::Kinds::standard(),
    })
}

/// Longest unix socket path, in bytes (`sun_path` less its NUL).
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

/// Parses the schedule store root.
///
/// Unset or empty means scheduling is off. Otherwise it must be absolute,
/// and apart from the credential vault's root: neither may contain the
/// other, so no vault grant can name a schedule-store entry.
/// The store root and its run root (`<root>.runs`) must each stay
/// apart from the vault root and out of the broker socket directory,
/// which the agent cell mounts: a run socket there would be reachable
/// from the agent cell.
fn parse_scheduler(
    dir: Option<&str>,
    model: (Option<String>, Option<String>),
    vault_root: &Path,
    socket_dir: Option<&Path>,
) -> Result<SchedulerConfig, Error> {
    let Some(dir) = dir.map(str::trim).filter(|dir| !dir.is_empty()) else {
        return Ok(SchedulerConfig::disabled());
    };
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_SCHEDULER_DIR",
        reason,
    };
    let root = PathBuf::from(dir);
    if !root.is_absolute() {
        return Err(invalid(format!(
            "scheduler dir must be an absolute path: {}",
            root.display()
        )));
    }
    let runs = root.with_extension("runs");
    for dir in [&root, &runs] {
        if dir.starts_with(vault_root) || vault_root.starts_with(dir) {
            return Err(invalid(format!(
                "scheduler dir {} overlaps the vault root {}: a vault grant could reach it",
                dir.display(),
                vault_root.display()
            )));
        }
        if socket_dir.is_some_and(|socket_dir| dir.starts_with(socket_dir)) {
            return Err(invalid(format!(
                "scheduler dir {} is inside the broker socket directory, which the cell mounts",
                dir.display()
            )));
        }
    }
    let (model_endpoint, model_name) = model;
    Ok(SchedulerConfig {
        root,
        model_endpoint: model_endpoint.filter(|value| !value.trim().is_empty()),
        model_name: model_name.filter(|value| !value.trim().is_empty()),
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        agent_bin: None,
        followup: None,
        subagent_depth: 0,
    })
}

fn parse_goals(dir: Option<String>, grants_raw: Option<String>) -> Result<GoalsConfig, Error> {
    let Some(dir) = dir.filter(|dir| !dir.trim().is_empty()) else {
        return Ok(GoalsConfig::disabled());
    };
    let root = PathBuf::from(dir);
    if !root.is_absolute() {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_GOALS_DIR",
            reason: format!("goals dir must be an absolute path: {}", root.display()),
        });
    }
    let mut grants = Vec::new();
    for entry in grants_raw.unwrap_or_default().split(',') {
        let action = match entry.trim() {
            "" => continue,
            "goal-create" => GoalsAction::GoalCreate,
            "goal-update" => GoalsAction::GoalUpdate,
            "goal-close" => GoalsAction::GoalClose,
            "tracking-append" => GoalsAction::TrackingAppend,
            "query-goals" => GoalsAction::QueryGoals,
            "query-tracking" => GoalsAction::QueryTracking,
            other => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_GOALS_GRANTS",
                    reason: format!("unknown goals action {other:?}"),
                });
            }
        };
        grants.push(action);
    }
    Ok(GoalsConfig { root, grants })
}

/// Parses the channel list: comma-separated channel ids.
///
/// Missing or blank means just the local channel. Entries are trimmed;
/// empty entries are rejected (fail closed — a typo must not silently
/// disable a channel).
fn parse_channels(raw: Option<String>) -> Result<Vec<String>, Error> {
    let Some(raw) = raw else {
        return Ok(vec![String::from("local")]);
    };
    if raw.trim().is_empty() {
        return Ok(vec![String::from("local")]);
    }
    raw.split(',')
        .map(|entry| {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                Err(Error::InvalidConfig {
                    key: "HATCHLING_CHANNELS",
                    reason: format!("empty channel id in {raw:?}"),
                })
            } else {
                Ok(trimmed.to_owned())
            }
        })
        .collect()
}

/// Parses the local channel's ingress socket path.
///
/// Unset or empty means no ingress. Otherwise the path must be absolute,
/// the local channel must be enabled, and the path must lie outside the
/// broker socket's directory: the cell bind-mounts that directory to
/// reach the broker, so an ingress inside it would be reachable from the
/// cell, which could then inject its own messages and bank reply credit.
fn parse_channel_ingress(
    raw: Option<&str>,
    broker_socket: &SocketPath,
    channels: &[String],
) -> Result<Option<SocketPath>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_CHANNEL_INGRESS",
        reason,
    };
    let ingress: SocketPath = raw
        .parse()
        .map_err(|_| invalid(format!("ingress socket must be an absolute path: {raw:?}")))?;
    if !channels.iter().any(|channel| channel == "local") {
        return Err(invalid(String::from(
            "the ingress feeds the local channel, which is not enabled",
        )));
    }
    if let Some(cell_visible) = broker_socket.as_path().parent() {
        if ingress.as_path().starts_with(cell_visible) {
            return Err(invalid(format!(
                "ingress {raw:?} is inside the broker socket directory {}, which the cell mounts",
                cell_visible.display()
            )));
        }
    }
    Ok(Some(ingress))
}

/// Longest account label.
const MAX_ACCOUNT_LABEL: usize = 32;

/// Parses the channel accounts: comma-separated `channel:label=secret`.
///
/// Fail-closed: the channel must be configured, the label must be
/// 1–[`MAX_ACCOUNT_LABEL`] of `a-z0-9-`, a (channel, label) pair may
/// appear once, and the secret must be named by no vault grant — a
/// granted name would let the cell fetch, store over, or remove the
/// credential the broker holds in custody.
fn parse_channel_accounts(
    raw: Option<&str>,
    channels: &[String],
    vault_grants: &[VaultGrant],
) -> Result<Vec<ChannelAccountSpec>, Error> {
    let mut accounts: Vec<ChannelAccountSpec> = Vec::new();
    for entry in raw.into_iter().flat_map(|raw| raw.split(',')) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let invalid = |reason: &str| Error::InvalidConfig {
            key: "HATCHLING_CHANNEL_ACCOUNTS",
            reason: format!("account {entry:?}: {reason}"),
        };
        let (route, secret) = entry
            .split_once('=')
            .ok_or_else(|| invalid("expected 'channel:label=secret'"))?;
        let (channel, label) = route
            .split_once(':')
            .ok_or_else(|| invalid("expected 'channel:label=secret'"))?;
        let (channel, label) = (channel.trim(), label.trim());
        if !channels.iter().any(|configured| configured == channel) {
            return Err(invalid("the channel is not configured"));
        }
        let label_ok = (1..=MAX_ACCOUNT_LABEL).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !label_ok {
            return Err(invalid("the label must be 1-32 of a-z, 0-9, '-'"));
        }
        let secret: SecretName = secret
            .trim()
            .parse()
            .map_err(|_| invalid("the secret is not a valid secret name"))?;
        if vault_grants.iter().any(|grant| grant.name == secret) {
            return Err(invalid(
                "a vault grant names the secret, so the cell could reach it",
            ));
        }
        if accounts
            .iter()
            .any(|account| account.channel == channel && account.label == label)
        {
            return Err(invalid("the account is configured twice"));
        }
        accounts.push(ChannelAccountSpec {
            channel: channel.to_owned(),
            label: label.to_owned(),
            secret,
        });
    }
    Ok(accounts)
}

/// Parses skill configuration from environment.
///
/// `HATCHLING_SKILLS_DIR`: directory containing skill manifests.
/// `HATCHLING_SKILL_TRUST_ROOTS`: absolute path of the trust-roots file.
/// `HATCHLING_SKILL_GRANTS`: `skill:capability` entries, comma-separated.
///
/// Empty skills dir means no skills. If a dir is set, the trust-roots
/// file path is required.
fn parse_skills(
    dir_raw: Option<String>,
    trust_raw: Option<String>,
    grants_raw: Option<String>,
) -> Result<SkillConfig, Error> {
    let Some(dir) = dir_raw else {
        return Ok(SkillConfig::disabled());
    };
    if dir.trim().is_empty() {
        return Ok(SkillConfig::disabled());
    }
    let skills_dir = PathBuf::from(&dir);
    if !skills_dir.is_absolute() {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_SKILLS_DIR",
            reason: format!(
                "skills dir must be an absolute path: {}",
                skills_dir.display()
            ),
        });
    }
    let trust_roots_file = PathBuf::from(
        trust_raw
            .ok_or(Error::MissingConfig("HATCHLING_SKILL_TRUST_ROOTS"))?
            .trim(),
    );
    if !trust_roots_file.is_absolute() {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_SKILL_TRUST_ROOTS",
            reason: format!(
                "the trust-roots file must be an absolute path: {}",
                trust_roots_file.display()
            ),
        });
    }
    let grants = parse_skill_grants(grants_raw)?;
    Ok(SkillConfig {
        skills_dir,
        trust_roots_file,
        grants,
    })
}

/// Parses skill grants: `skill:capability` entries, comma-separated.
///
/// Every capability parses as a [`CapabilityName`] here, once, at
/// startup: an empty, overlong, or out-of-alphabet name stops the broker
/// with the offending entry named. A typo'd grant must never load as a
/// grant that silently matches nothing.
fn parse_skill_grants(
    raw: Option<String>,
) -> Result<HashMap<String, HashSet<CapabilityName>>, Error> {
    let mut grants: HashMap<String, HashSet<CapabilityName>> = HashMap::new();
    let Some(raw) = raw else {
        return Ok(grants);
    };
    if raw.trim().is_empty() {
        return Ok(grants);
    }
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        let invalid = |reason: String| Error::InvalidConfig {
            key: "HATCHLING_SKILL_GRANTS",
            reason,
        };
        let Some((skill, cap)) = trimmed.split_once(':') else {
            return Err(invalid(format!(
                "expected skill:capability, got {trimmed:?}"
            )));
        };
        let skill = skill.trim();
        if skill.is_empty() {
            return Err(invalid(format!("empty skill name in {trimmed:?}")));
        }
        let capability = cap
            .trim()
            .parse::<CapabilityName>()
            .map_err(|err| invalid(format!("{trimmed:?}: {err}")))?;
        grants
            .entry(skill.to_owned())
            .or_default()
            .insert(capability);
    }
    Ok(grants)
}

/// Parses the grant list: `action:name` entries separated by commas.
///
/// Missing or blank means no grants — deny-by-default. A malformed entry
/// fails closed: a typo'd grant must never silently become a missing
/// grant, in either direction. No grant may name a real CLI harness's
/// vendor key.
fn parse_grants(raw: Option<String>) -> Result<Vec<VaultGrant>, Error> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|entry| {
            let entry = entry.trim();
            let invalid = |reason: String| Error::InvalidConfig {
                key: "HATCHLING_VAULT_GRANTS",
                reason: format!("grant {entry:?}: {reason}"),
            };
            let (action, name) = entry
                .split_once(':')
                .ok_or_else(|| invalid(String::from("expected 'action:name'")))?;
            // `?` maps core parse failures onto `InvalidConfig` for this
            // key via the `From` impl: grant parsing is the only place
            // these variants can arise in the broker.
            let action: VaultAction = action.trim().parse()?;
            let name: SecretName = name.trim().parse()?;
            Ok(VaultGrant { action, name })
        })
        .collect::<Result<_, _>>()
        .and_then(refuse_harness_secret_grants)
}

/// The default connection cap.
///
/// How it was chosen: the broker serves one agent cell, which holds one
/// session at a time. A reconnect can overlap the old session's teardown
/// (2), and operator tooling may hold a session alongside (3). Headroom
/// on top gives 16 — well past any legitimate use, and small enough
/// that a runaway peer is capped at 16 threads (about 32 MiB of reserved
/// stack at the default size) instead of unbounded. The channel ingress
/// has its own socket and does not count.
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;

/// The `nobody` uid, which every process running as nobody shares.
pub const NOBODY_UID: u32 = 65534;

/// `HATCHLING_AGENT_UID`, which must not be `nobody` unless
/// `HATCHLING_AGENT_ALLOW_NOBODY` says so ([`refuse_shared_agent_uid`]).
///
/// With `HATCHLING_CELL_IDS=subordinate` it may be left out: the agent's
/// uid is then the one its cells run as, the owner's first subordinate
/// uid, which `subordinate` reads (WO-014). A value given is checked as
/// usual, and the cell's own config refuses one that isn't that uid.
fn parse_agent_uid(
    get: &impl Fn(&str) -> Option<String>,
    subordinate: impl FnOnce() -> Result<u32, Error>,
) -> Result<PeerUid, Error> {
    let agent_uid = match get(env::HATCHLING_AGENT_UID.name) {
        Some(raw) => {
            raw.parse::<u32>()
                .map(PeerUid::from_kernel)
                .map_err(|_| Error::InvalidConfig {
                    key: "HATCHLING_AGENT_UID",
                    reason: String::from("expected a kernel uid as a base-10 u32"),
                })?
        }
        None if CellIds::parse(get)? == CellIds::Subordinate => {
            PeerUid::from_kernel(subordinate()?)
        }
        None => return Err(Error::MissingConfig("HATCHLING_AGENT_UID")),
    };
    refuse_shared_agent_uid(
        agent_uid,
        get(env::HATCHLING_AGENT_ALLOW_NOBODY.name).as_deref(),
    )?;
    Ok(agent_uid)
}

/// Refuses `nobody` as the agent uid unless `allow` (the
/// `HATCHLING_AGENT_ALLOW_NOBODY` setting) says to accept it. The broker
/// admits an agent by its uid, so any other process running as nobody
/// could act as the agent.
fn refuse_shared_agent_uid(agent_uid: PeerUid, allow: Option<&str>) -> Result<(), Error> {
    let allowed = crate::api_auth::flag(allow, "HATCHLING_AGENT_ALLOW_NOBODY")?;
    if agent_uid.as_u32() == NOBODY_UID && !allowed {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_AGENT_UID",
            reason: format!(
                "{NOBODY_UID} is nobody, which other processes share, and any of them could act \
                 as the agent: give cells a uid of their own, or set \
                 HATCHLING_AGENT_ALLOW_NOBODY=true to accept that"
            ),
        });
    }
    Ok(())
}

/// The largest cap the knob accepts. A cap is only useful if it bounds
/// something: a typo like `1000000` fails startup instead of quietly
/// meaning "uncapped".
pub const MAX_MAX_CONNECTIONS: usize = 1024;

/// Parses the connection cap: a base-10 integer in
/// 1–[`MAX_MAX_CONNECTIONS`], or [`DEFAULT_MAX_CONNECTIONS`] when unset.
/// Never silently uncapped.
fn parse_max_connections(raw: Option<&str>) -> Result<usize, Error> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_MAX_CONNECTIONS);
    };
    let invalid = |reason: &str| Error::InvalidConfig {
        key: "HATCHLING_BROKER_MAX_CONNECTIONS",
        reason: format!("{reason}: {raw:?}"),
    };
    let cap: usize = raw
        .trim()
        .parse()
        .map_err(|_| invalid("expected a base-10 connection count"))?;
    if cap == 0 {
        return Err(invalid("a cap of zero would refuse every connection"));
    }
    if cap > MAX_MAX_CONNECTIONS {
        return Err(invalid("the cap exceeds 1024"));
    }
    Ok(cap)
}

/// Parses an octal socket mode, rejecting anything outside `0o000`–`0o777`.
fn parse_socket_mode(raw: &str) -> Result<u32, Error> {
    let invalid = |reason: &str| Error::InvalidConfig {
        key: "HATCHLING_BROKER_SOCKET_MODE",
        reason: format!("{reason}: {raw:?}"),
    };
    let mode = u32::from_str_radix(raw, 8).map_err(|_| invalid("expected an octal mode"))?;
    if mode > 0o777 {
        return Err(invalid("mode exceeds 0o777"));
    }
    Ok(mode)
}

/// Refuses any vault grant that names a broker-held egress credential
/// .
///
/// The broker reads an egress destination's credential from the vault
/// secret [`destination_to_secret_name`](crate::serve::destination_to_secret_name)
/// gives for it. A vault grant naming that secret would let the cell
/// fetch it (disclosure), store over it (replacement), or remove it
/// (destruction) through the vault ops, so any grant action on it fails
/// startup — as refuses it for channel accounts.
///
/// Protected are the secrets of every destination the operator named:
/// each egress grant and each allowlist entry. The broker only reads a
/// credential for a destination on both, so this is a superset of what
/// it can read. The names come from the broker's own lookup mapping,
/// never a copy of it: that mapping is lossy (`api.example:443` and
/// `api-example:443` share `egress-api-example-443`), and the check
/// must protect exactly the names the lookup reads.
fn refuse_egress_secret_grants(
    vault_grants: &[VaultGrant],
    egress_grants: &[String],
    allowlist_raw: &str,
) -> Result<(), Error> {
    for destination in configured_destinations(egress_grants, allowlist_raw) {
        let secret = crate::serve::destination_to_secret_name(destination);
        if let Some(grant) = vault_grants
            .iter()
            .find(|grant| grant.name.as_str() == secret)
        {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_VAULT_GRANTS",
                reason: format!(
                    "grant {}:{} names the credential the broker holds for egress destination {destination:?}",
                    grant.action,
                    grant.name.as_str()
                ),
            });
        }
    }
    Ok(())
}

/// The checks on the vault secrets that hold egress credentials: no vault
/// grant may name one, and no two destinations may share one.
fn refuse_egress_secret_conflicts(
    vault_grants: &[VaultGrant],
    egress_grants: &[String],
    allowlist_raw: &str,
) -> Result<(), Error> {
    refuse_egress_secret_grants(vault_grants, egress_grants, allowlist_raw)?;
    refuse_shared_egress_secrets(egress_grants, allowlist_raw)
}

/// Every egress destination the operator named: each egress grant, and
/// each allowlist entry without its `public:`, `local:`,
/// `local-keyless:` or `internal:` prefix. The credential lookup maps the bare
/// destination (`localhost:11434` reads `egress-localhost-11434`), so the
/// checks on secret names must map it bare too.
fn configured_destinations<'a>(
    egress_grants: &'a [String],
    allowlist_raw: &'a str,
) -> Vec<&'a str> {
    let unprefixed = |entry: &'a str| {
        [
            hatchling_egress::PUBLIC_PREFIX,
            hatchling_egress::LOCAL_KEYLESS_PREFIX,
            hatchling_egress::LOCAL_PREFIX,
            hatchling_egress::INTERNAL_PREFIX,
        ]
        .iter()
        .find_map(|prefix| entry.strip_prefix(prefix))
        .unwrap_or(entry)
    };
    egress_grants
        .iter()
        .map(String::as_str)
        .chain(
            allowlist_raw
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(unprefixed),
        )
        .collect()
}

/// Refuses two different egress destinations that would share a vault
/// secret: the lookup mapping is lossy (`api.example.com:443` and
/// `api-example.com:443` both read `egress-api-example-com-443`), so
/// one host would be sent the other's credential. The same destination
/// named twice (as a grant and on the allowlist) is fine.
fn refuse_shared_egress_secrets(
    egress_grants: &[String],
    allowlist_raw: &str,
) -> Result<(), Error> {
    let mut owners = std::collections::BTreeMap::new();
    for destination in configured_destinations(egress_grants, allowlist_raw) {
        let secret = crate::serve::destination_to_secret_name(destination);
        match owners.get(&secret) {
            Some(&owner) if owner != destination => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_EGRESS_ALLOWLIST",
                    reason: format!(
                        "egress destinations {owner:?} and {destination:?} would share the credential {secret}, so one would be sent the other's key; rename one or drop one"
                    ),
                });
            }
            Some(_) => {}
            None => {
                owners.insert(secret, destination);
            }
        }
    }
    Ok(())
}

/// Refuses a vault grant naming a harness's vendor key: the
/// broker reads it for the harness's relay, and a grant would hand it to
/// the agent's cell, defeating the surrogate. Other grants pass through.
fn refuse_harness_secret_grants(vault_grants: Vec<VaultGrant>) -> Result<Vec<VaultGrant>, Error> {
    for kind in crate::harness::HarnessKind::ALL {
        if let Some(grant) = vault_grants
            .iter()
            .find(|grant| grant.name.as_str() == kind.secret_name())
        {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_VAULT_GRANTS",
                reason: format!(
                    "grant {}:{} names the vendor key the broker holds for the {} harness",
                    grant.action,
                    grant.name.as_str(),
                    kind.name()
                ),
            });
        }
    }
    if let Some(grant) = vault_grants
        .iter()
        .find(|grant| grant.name.as_str() == crate::web_ops::SEARCH_KEY)
    {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_VAULT_GRANTS",
            reason: format!(
                "grant {}:{} names the search provider's key, which only the egress authority reads",
                grant.action,
                grant.name.as_str()
            ),
        });
    }
    if let Some(grant) = vault_grants
        .iter()
        .find(|grant| grant.name.as_str() == crate::keeper::RELEASED_RECORD)
    {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_VAULT_GRANTS",
            reason: format!(
                "grant {}:{} names the vault keeper's record of what it has released",
                grant.action,
                grant.name.as_str()
            ),
        });
    }
    Ok(vault_grants)
}

/// Parses the egress destination grants: comma-separated `host:port`.
///
/// Missing or blank means no grants — deny-by-default. Each entry must
/// be a valid destination (explicit host:port, no wildcards, no IP
/// literals); a malformed entry fails closed.
fn parse_egress_grants(raw: Option<String>) -> Result<Vec<String>, Error> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|entry| {
            let entry = entry.trim().to_owned();
            // Validate the destination format now, at startup. The
            // allowlist check happens at request time; this catches
            // typos early.
            if entry.is_empty() {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_EGRESS_GRANTS",
                    reason: String::from("empty destination in grant list"),
                });
            }
            // Basic format check: must contain a colon separating host
            // and port. Full validation happens in the egress crate.
            if !entry.contains(':') {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_EGRESS_GRANTS",
                    reason: format!("grant {entry:?}: expected 'host:port'"),
                });
            }
            Ok(entry)
        })
        .collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn thinking_mode_refuses_nonunicode_environment_before_startup() {
        use std::os::unix::ffi::OsStringExt as _;
        const CHILD: &str = "HATCHLING_TEST_REASONING_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let error = Config::from_env().unwrap_err();
            assert!(
                matches!(error, Error::InvalidConfig { key: "HATCHLING_SCHEDULER_MODEL_REASONING", reason } if reason == "expected provider-default or disabled")
            );
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::thinking_mode_refuses_nonunicode_environment_before_startup",
                "--nocapture",
            ])
            .env_clear()
            .envs(valid_vars())
            .env(CHILD, "1")
            .env(
                "HATCHLING_SCHEDULER_MODEL_REASONING",
                std::ffi::OsString::from_vec(vec![0xff]),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    fn another_settings_nonunicode_value_still_reads_as_unset() {
        // Only the thinking mode refuses a non-UTF-8 value; any other
        // setting's reads as unset, as before WO-020. An unset socket mode
        // is the default, and an empty one would be refused.
        use std::os::unix::ffi::OsStringExt as _;
        const CHILD: &str = "HATCHLING_TEST_OTHER_NONUNICODE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let config = Config::from_env().unwrap();
            assert_eq!(config.socket_mode, DEFAULT_SOCKET_MODE);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::another_settings_nonunicode_value_still_reads_as_unset",
                "--nocapture",
            ])
            .env_clear()
            .envs(valid_vars())
            .env(CHILD, "1")
            .env(
                "HATCHLING_BROKER_SOCKET_MODE",
                std::ffi::OsString::from_vec(vec![0xff]),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    use super::*;
    use std::collections::HashMap;

    /// Builds a lookup closure over a map, so tests never touch process env.
    fn lookup<'a>(vars: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| vars.get(key).map(|value| (*value).to_owned())
    }

    fn valid_vars() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("HATCHLING_BROKER_SOCKET", "/tmp/hatchling-test/broker.sock"),
            ("HATCHLING_AGENT_UID", "1000"),
            ("HATCHLING_VAULT_DIR", "/tmp/hatchling-test/vault"),
            (
                "HATCHLING_VAULT_GRANTS",
                "fetch:app-token,store:app-token,remove:old-key",
            ),
        ])
    }

    #[test]
    fn parse_stores_routes_nondefault_registered_settings() {
        let mut vars = valid_vars();
        vars.extend([
            ("HATCHLING_MEMORY_DIR", "/srv/hatchling/memory"),
            ("HATCHLING_MEMORY_GRANTS", "user:write"),
            ("HATCHLING_GOALS_DIR", "/srv/hatchling/goals"),
            ("HATCHLING_GOALS_GRANTS", "goal-create"),
            ("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/schedules"),
            (
                "HATCHLING_SCHEDULER_MODEL_ENDPOINT",
                "http://127.0.0.1:8000",
            ),
            ("HATCHLING_SCHEDULER_MODEL_NAME", "stand-in"),
            ("HATCHLING_AGENT_BIN", "/usr/local/bin/hatchling"),
            ("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts"),
            ("HATCHLING_SPACE_DIR", "/srv/hatchling/spaces"),
        ]);
        let base = Config::parse(lookup(&valid_vars())).unwrap();
        let (memory, goals, scheduler, artifacts, spaces) =
            parse_stores(&lookup(&vars), &base.vault.root, &base.socket).unwrap();
        assert_eq!(memory.root, Path::new("/srv/hatchling/memory"));
        assert_eq!(memory.grants.len(), 1);
        assert_eq!(goals.root, Path::new("/srv/hatchling/goals"));
        assert_eq!(goals.grants, vec![GoalsAction::GoalCreate]);
        assert_eq!(scheduler.root, Path::new("/srv/hatchling/schedules"));
        assert_eq!(
            scheduler.model_endpoint.as_deref(),
            Some("http://127.0.0.1:8000")
        );
        assert_eq!(scheduler.model_name.as_deref(), Some("stand-in"));
        assert_eq!(
            scheduler.agent_bin,
            Some(PathBuf::from("/usr/local/bin/hatchling"))
        );
        assert_eq!(artifacts.root, Path::new("/srv/hatchling/artifacts"));
        assert_eq!(spaces.root, Path::new("/srv/hatchling/spaces"));
    }

    #[test]
    fn parse_artifacts_reads_registered_policy_and_refuses_a_vault_overlap() {
        let mut vars = valid_vars();
        vars.extend([
            ("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts"),
            ("HATCHLING_ARTIFACT_WORKSPACE", "/home/user/output"),
            ("HATCHLING_ARTIFACT_MAX_BYTES", "4096"),
            ("HATCHLING_ARTIFACT_DENY", " secret, token "),
        ]);
        let vault = Path::new("/tmp/hatchling-test/vault");
        let socket_dir = Some(Path::new("/tmp/hatchling-test"));
        let parsed = parse_artifacts(&lookup(&vars), vault, socket_dir).unwrap();
        assert_eq!(parsed.root, Path::new("/srv/hatchling/artifacts"));
        assert_eq!(parsed.workspace, Some(PathBuf::from("/home/user/output")));
        let policy = parsed.policy.unwrap();
        assert_eq!(policy.max_bytes, 4096);
        assert_eq!(policy.deny, vec!["secret", "token"]);
        vars.insert("HATCHLING_ARTIFACT_DIR", "/tmp/hatchling-test/vault/output");
        assert!(matches!(
            parse_artifacts(&lookup(&vars), vault, socket_dir),
            Err(Error::InvalidConfig {
                key: "HATCHLING_ARTIFACT_DIR",
                ..
            })
        ));
    }

    #[test]
    fn artifacts_are_off_unless_a_store_is_configured() {
        let config = Config::parse(lookup(&valid_vars())).unwrap();
        assert!(!config.artifacts.enabled());
        assert_eq!(config.artifacts.workspace, None);
        assert_eq!(config.artifacts.policy, None);
        let mut vars = valid_vars();
        vars.insert("HATCHLING_ARTIFACT_DIR", "  ");
        assert!(!Config::parse(lookup(&vars)).unwrap().artifacts.enabled());
    }

    #[test]
    fn the_artifact_store_workspace_and_policy_parse() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
        vars.insert("HATCHLING_ARTIFACT_WORKSPACE", "/home/user/out");
        vars.insert("HATCHLING_ARTIFACT_MAX_BYTES", "4096");
        vars.insert("HATCHLING_ARTIFACT_DENY", " CONFIDENTIAL , ,internal-only");
        let artifacts = Config::parse(lookup(&vars)).unwrap().artifacts;
        assert!(artifacts.enabled());
        assert_eq!(artifacts.root, PathBuf::from("/srv/hatchling/artifacts"));
        assert_eq!(artifacts.workspace, Some(PathBuf::from("/home/user/out")));
        assert_eq!(
            artifacts.policy,
            Some(hatchling_artifact::ReviewPolicy {
                max_bytes: 4096,
                deny: vec![String::from("CONFIDENTIAL"), String::from("internal-only")],
            })
        );
        // No size, no deny list: no policy, so the gate refuses everything.
        vars.remove("HATCHLING_ARTIFACT_MAX_BYTES");
        vars.remove("HATCHLING_ARTIFACT_DENY");
        let artifacts = Config::parse(lookup(&vars)).unwrap().artifacts;
        assert_eq!(artifacts.policy, None);
        assert!(artifacts.enabled());
    }

    #[test]
    fn the_space_store_is_off_unless_configured_and_never_bound_from_the_env() {
        let config = Config::parse(lookup(&valid_vars())).unwrap();
        assert!(!config.spaces.enabled());
        assert_eq!(config.spaces.root, PathBuf::new());
        let mut vars = valid_vars();
        vars.insert("HATCHLING_SPACE_DIR", " ");
        assert!(!Config::parse(lookup(&vars)).unwrap().spaces.enabled());
        vars.insert("HATCHLING_SPACE_DIR", " /srv/hatchling/spaces ");
        vars.insert("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/schedules");
        vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
        let spaces = Config::parse(lookup(&vars)).unwrap().spaces;
        assert!(spaces.enabled());
        assert_eq!(spaces.root, PathBuf::from("/srv/hatchling/spaces"));
        assert_eq!(spaces.bound, None, "only scoped_config binds a session");
        assert!(!SpaceConfig::disabled().enabled());
        assert_eq!(SpaceConfig::disabled().bound, None);
    }

    #[test]
    fn the_device_socket_needs_spaces_and_stays_out_of_the_cell() {
        let mut vars = valid_vars();
        assert!(
            Config::parse(lookup(&vars))
                .unwrap()
                .spaces
                .device_socket
                .is_none()
        );
        vars.insert("HATCHLING_SPACE_DIR", "/srv/hatchling/spaces");
        vars.insert("HATCHLING_DEVICE_SOCKET", " ");
        assert!(
            Config::parse(lookup(&vars))
                .unwrap()
                .spaces
                .device_socket
                .is_none()
        );
        vars.insert("HATCHLING_DEVICE_SOCKET", " /run/hatchling/devices.sock ");
        let spaces = Config::parse(lookup(&vars)).unwrap().spaces;
        assert_eq!(
            spaces.device_socket.map(|s| s.as_path().to_path_buf()),
            Some(PathBuf::from("/run/hatchling/devices.sock"))
        );
        let refused = |pairs: &[(&'static str, &'static str)]| {
            let mut vars = valid_vars();
            for (k, v) in pairs {
                vars.insert(k, v);
            }
            let result = Config::parse(lookup(&vars));
            assert!(
                matches!(
                    result,
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_DEVICE_SOCKET",
                        ..
                    })
                ),
                "{pairs:?} gave {result:?}"
            );
        };
        // No space store: nothing for a device to sync.
        refused(&[("HATCHLING_DEVICE_SOCKET", "/run/hatchling/devices.sock")]);
        // Relative, or where the cell can reach it.
        for socket in ["devices.sock", "/tmp/hatchling-test/devices.sock"] {
            refused(&[
                ("HATCHLING_SPACE_DIR", "/srv/hatchling/spaces"),
                ("HATCHLING_DEVICE_SOCKET", socket),
            ]);
        }
    }

    #[test]
    fn a_misplaced_space_store_is_refused() {
        let refused = |pairs: &[(&'static str, &'static str)]| {
            let mut vars = valid_vars();
            for (k, v) in pairs {
                vars.insert(k, v);
            }
            let result = Config::parse(lookup(&vars));
            assert!(
                matches!(
                    result,
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_SPACE_DIR",
                        ..
                    })
                ),
                "{pairs:?} gave {result:?}"
            );
        };
        refused(&[("HATCHLING_SPACE_DIR", "spaces")]);
        // Inside the vault, or containing it.
        refused(&[("HATCHLING_SPACE_DIR", "/tmp/hatchling-test/vault/s")]);
        refused(&[("HATCHLING_SPACE_DIR", "/tmp")]);
        // Inside the socket dir the cell mounts.
        refused(&[("HATCHLING_SPACE_DIR", "/tmp/hatchling-test/s")]);
        // Overlapping the schedule store or the build store, either way.
        for (store, dir) in [
            ("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/s/schedules"),
            ("HATCHLING_SCHEDULER_DIR", "/srv"),
            ("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/s/artifacts"),
            ("HATCHLING_ARTIFACT_DIR", "/srv"),
        ] {
            refused(&[("HATCHLING_SPACE_DIR", "/srv/hatchling/s"), (store, dir)]);
        }
        // Inside the vault while the other stores are configured too: the
        // vault alone decides it.
        refused(&[
            ("HATCHLING_SPACE_DIR", "/tmp/hatchling-test/vault/s"),
            ("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/schedules"),
            ("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts"),
        ]);
        refused(&[
            ("HATCHLING_SPACE_DIR", "/tmp"),
            ("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/schedules"),
            ("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts"),
        ]);
        // Apart from all of them, it parses; a store that is off is no
        // store to overlap.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_SPACE_DIR", "/srv/hatchling/s");
        assert!(Config::parse(lookup(&vars)).is_ok(), "the other stores off");
        vars.insert("HATCHLING_SCHEDULER_DIR", "/srv/hatchling/schedules");
        vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
        assert!(Config::parse(lookup(&vars)).is_ok());
    }

    #[test]
    fn a_misplaced_artifact_store_or_bad_policy_is_refused() {
        let refused = |pairs: &[(&'static str, &'static str)], key: &str| {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
            vars.insert("HATCHLING_ARTIFACT_MAX_BYTES", "4096");
            for (k, v) in pairs {
                vars.insert(k, v);
            }
            match Config::parse(lookup(&vars)) {
                Err(Error::InvalidConfig { key: got, .. }) => assert_eq!(got, key, "{pairs:?}"),
                other => assert!(other.is_err(), "{pairs:?} parsed: {other:?}"),
            }
        };
        refused(
            &[("HATCHLING_ARTIFACT_DIR", "artifacts")],
            "HATCHLING_ARTIFACT_DIR",
        );
        // Inside the vault, or containing it.
        refused(
            &[("HATCHLING_ARTIFACT_DIR", "/tmp/hatchling-test/vault/a")],
            "HATCHLING_ARTIFACT_DIR",
        );
        refused(
            &[("HATCHLING_ARTIFACT_DIR", "/tmp")],
            "HATCHLING_ARTIFACT_DIR",
        );
        // Inside the socket dir the cell mounts.
        refused(
            &[("HATCHLING_ARTIFACT_DIR", "/tmp/hatchling-test/a")],
            "HATCHLING_ARTIFACT_DIR",
        );
        refused(
            &[("HATCHLING_ARTIFACT_WORKSPACE", "out")],
            "HATCHLING_ARTIFACT_WORKSPACE",
        );
        refused(
            &[("HATCHLING_ARTIFACT_WORKSPACE", "/tmp/hatchling-test/out")],
            "HATCHLING_ARTIFACT_WORKSPACE",
        );
        refused(
            &[("HATCHLING_ARTIFACT_WORKSPACE", "/tmp/hatchling-test/vault")],
            "HATCHLING_ARTIFACT_WORKSPACE",
        );
        // Inside the store, or containing it.
        refused(
            &[(
                "HATCHLING_ARTIFACT_WORKSPACE",
                "/srv/hatchling/artifacts/out",
            )],
            "HATCHLING_ARTIFACT_WORKSPACE",
        );
        refused(
            &[("HATCHLING_ARTIFACT_WORKSPACE", "/srv")],
            "HATCHLING_ARTIFACT_WORKSPACE",
        );
        refused(
            &[("HATCHLING_ARTIFACT_MAX_BYTES", "0")],
            "HATCHLING_ARTIFACT_MAX_BYTES",
        );
        refused(
            &[("HATCHLING_ARTIFACT_MAX_BYTES", "-1")],
            "HATCHLING_ARTIFACT_MAX_BYTES",
        );
        refused(
            &[("HATCHLING_ARTIFACT_MAX_BYTES", "lots")],
            "HATCHLING_ARTIFACT_MAX_BYTES",
        );
        let mut vars = valid_vars();
        vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
        vars.insert("HATCHLING_ARTIFACT_DENY", "x");
        assert!(matches!(
            Config::parse(lookup(&vars)),
            Err(Error::InvalidConfig {
                key: "HATCHLING_ARTIFACT_DENY",
                ..
            })
        ));
        // Apart, and a workspace beside the store: fine.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_ARTIFACT_DIR", "/srv/hatchling/artifacts");
        vars.insert(
            "HATCHLING_ARTIFACT_WORKSPACE",
            "/srv/hatchling/artifacts-out",
        );
        assert!(Config::parse(lookup(&vars)).is_ok());
    }

    #[test]
    fn the_inline_trust_root_variable_is_refused_by_name() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_SKILL_TRUST_ROOT", "AAAA");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidConfig { key: "HATCHLING_SKILL_TRUST_ROOT", reason }
                if reason.contains("HATCHLING_SKILL_TRUST_ROOTS")),
            "{err:?}"
        );
        vars.remove("HATCHLING_SKILL_TRUST_ROOT");
        vars.insert("HATCHLING_SKILLS_DIR", "/srv/skills");
        vars.insert("HATCHLING_SKILL_TRUST_ROOTS", "/etc/hatchling/trust-roots");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(
            config.skills.trust_roots_file,
            PathBuf::from("/etc/hatchling/trust-roots")
        );
    }

    fn accounts(
        raw: &str,
        channels: &[&str],
        grants: &str,
    ) -> Result<Vec<ChannelAccountSpec>, Error> {
        let channels: Vec<String> = channels.iter().map(|c| (*c).to_owned()).collect();
        let grants = parse_grants(Some(grants.to_owned())).unwrap();
        parse_channel_accounts(Some(raw), &channels, &grants)
    }

    /// The accounts-config refusal's reason, or `None` for anything
    /// else — including a parse that wrongly succeeded, whose `Debug`
    /// would otherwise contain words like `label` and `secret`.
    fn account_error(raw: &str, channels: &[&str], grants: &str) -> Option<String> {
        match accounts(raw, channels, grants) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_CHANNEL_ACCOUNTS",
                reason,
            }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    fn refused_for(reason: Option<&str>, why: &str) -> bool {
        reason.is_some_and(|reason| reason.contains(why))
    }

    #[test]
    fn channel_accounts_parse_to_specs() {
        let parsed = accounts(
            " mail:work=mail-work , mail:personal-2=mail-home,,chat:bot=chat-bot ",
            &["local", "mail", "chat"],
            "fetch:app-token",
        )
        .unwrap();
        let triples: Vec<(&str, &str, &str)> = parsed
            .iter()
            .map(|a| (a.channel.as_str(), a.label.as_str(), a.secret.as_str()))
            .collect();
        assert_eq!(
            triples,
            [
                ("mail", "work", "mail-work"),
                ("mail", "personal-2", "mail-home"),
                ("chat", "bot", "chat-bot"),
            ]
        );
        assert!(
            parse_channel_accounts(None, &[], &[]).unwrap().is_empty(),
            "no key means no accounts"
        );
        assert!(accounts("  ", &["mail"], "").unwrap().is_empty());
        let label = "a".repeat(MAX_ACCOUNT_LABEL);
        assert_eq!(
            accounts(&format!("mail:{label}=s"), &["mail"], "")
                .unwrap()
                .len(),
            1,
            "a label at the bound is accepted"
        );
    }

    #[test]
    fn a_channel_account_secret_no_grant_may_name() {
        // Every grant action on the custody secret fails startup: fetch
        // would hand the cell the key, store and remove would let it
        // replace or destroy it.
        for grant in ["fetch:mail-work", "store:mail-work", "remove:mail-work"] {
            let reason = account_error("mail:work=mail-work", &["mail"], grant);
            assert!(
                refused_for(reason.as_deref(), "vault grant"),
                "{grant}: {reason:?}"
            );
        }
    }

    #[test]
    fn malformed_channel_accounts_fail_closed() {
        for (raw, why) in [
            ("mail-work", "expected"),
            ("mail=mail-work", "expected"),
            ("sms:work=s", "not configured"),
            ("mail:=s", "label"),
            ("mail:Work=s", "label"),
            ("mail:wo rk=s", "label"),
            ("mail:wo_rk=s", "label"),
            ("mail:work=../etc", "secret name"),
            ("mail:work=s,mail:work=t", "twice"),
        ] {
            let reason = account_error(raw, &["mail"], "");
            assert!(refused_for(reason.as_deref(), why), "{raw}: {reason:?}");
        }
        let too_long = format!("mail:{}=s", "a".repeat(MAX_ACCOUNT_LABEL + 1));
        let reason = account_error(&too_long, &["mail"], "");
        assert!(refused_for(reason.as_deref(), "label"), "{reason:?}");
        assert_eq!(MAX_ACCOUNT_LABEL, 32);
        // One label on two channels is two accounts.
        assert_eq!(
            accounts("mail:work=s,chat:work=t", &["mail", "chat"], "")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn config_carries_the_channel_accounts() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_CHANNELS", "local,mail");
        vars.insert("HATCHLING_CHANNEL_ACCOUNTS", "mail:work=mail-work");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.channel_accounts.len(), 1);
        vars.insert("HATCHLING_VAULT_GRANTS", "fetch:mail-work");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::InvalidConfig {
                    key: "HATCHLING_CHANNEL_ACCOUNTS",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// The vault-grants refusal for `vault_grants` against the egress
    /// grants and allowlist, or `None` if parsing did not refuse with
    /// `HATCHLING_VAULT_GRANTS`.
    fn egress_overlap(vault_grants: &str, egress_grants: &str, allowlist: &str) -> Option<String> {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_VAULT_GRANTS", vault_grants);
        vars.insert("HATCHLING_EGRESS_GRANTS", egress_grants);
        vars.insert("HATCHLING_EGRESS_ALLOWLIST", allowlist);
        match Config::parse(lookup(&vars)) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_VAULT_GRANTS",
                reason,
            }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    fn refuses(reason: Option<&str>, grant: &str) -> bool {
        reason.is_some_and(|reason| {
            reason.contains(&format!("grant {grant} names the credential"))
                && reason.contains("\"api.example.com:443\"")
        })
    }

    #[test]
    fn a_fetch_grant_on_an_egress_credential_is_refused() {
        // Fetch would disclose the credential to the cell.
        let reason = egress_overlap(
            "fetch:egress-api-example-com-443",
            "api.example.com:443",
            "api.example.com:443",
        );
        assert!(
            refuses(reason.as_deref(), "fetch:egress-api-example-com-443"),
            "{reason:?}"
        );
    }

    #[test]
    fn a_store_grant_on_an_egress_credential_is_refused() {
        // Store would let the cell replace the credential.
        let reason = egress_overlap(
            "store:egress-api-example-com-443",
            "api.example.com:443",
            "api.example.com:443",
        );
        assert!(
            refuses(reason.as_deref(), "store:egress-api-example-com-443"),
            "{reason:?}"
        );
    }

    #[test]
    fn a_remove_grant_on_an_egress_credential_is_refused() {
        // Remove would let the cell destroy the credential.
        let reason = egress_overlap(
            "remove:egress-api-example-com-443",
            "api.example.com:443",
            "api.example.com:443",
        );
        assert!(
            refuses(reason.as_deref(), "remove:egress-api-example-com-443"),
            "{reason:?}"
        );
    }

    #[test]
    fn a_prefixed_allowlist_entry_protects_the_name_the_lookup_reads() {
        // `local:localhost:11434` is looked up as `egress-localhost-11434`;
        // mapping it with its prefix would protect a name nothing reads.
        assert!(
            egress_overlap("fetch:egress-localhost-11434", "", "local:localhost:11434").is_some()
        );
        assert!(
            egress_overlap(
                "fetch:egress-api-example-com-443",
                "",
                "public:api.example.com:443"
            )
            .is_some()
        );
        assert!(
            egress_overlap(
                "fetch:egress-localhost-11434",
                "",
                "local-keyless:localhost:11434"
            )
            .is_some()
        );
    }

    /// The reason a config is refused for its egress destinations, if it is.
    fn egress_conflict(egress_grants: &str, allowlist: &str) -> Option<String> {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_EGRESS_GRANTS", egress_grants);
        vars.insert("HATCHLING_EGRESS_ALLOWLIST", allowlist);
        match Config::parse(lookup(&vars)) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_EGRESS_ALLOWLIST",
                reason,
            }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    #[test]
    fn two_destinations_that_would_share_a_credential_are_refused() {
        let reason = egress_conflict("", "api.example.com:443, api-example.com:443")
            .expect("a shared credential name is refused");
        assert!(
            reason.contains("\"api.example.com:443\"")
                && reason.contains("\"api-example.com:443\"")
                && reason.contains("egress-api-example-com-443"),
            "{reason}"
        );
        // Across a grant and the allowlist, and through a prefix.
        assert!(egress_conflict("api.example:443", "public:api-example:443").is_some());
        assert!(egress_conflict("a.b:443,a-b:443", "").is_some());
        // The same destination named more than once is one destination.
        assert!(
            egress_conflict(
                "api.example.com:443",
                "public:api.example.com:443, api.example.com:443"
            )
            .is_none()
        );
        // Distinct names configure.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_EGRESS_GRANTS", "api.example.com:443");
        vars.insert(
            "HATCHLING_EGRESS_ALLOWLIST",
            "api.example.com:443, api.example.org:443",
        );
        assert!(Config::parse(lookup(&vars)).is_ok());
    }

    #[test]
    fn the_egress_allowlist_is_resolved_once_and_shared_by_every_copy() {
        let allowlist = EgressAllowlist::new("internal:localhost:443");
        assert_eq!(allowlist.raw(), "internal:localhost:443");
        let copy = allowlist.clone();
        let first = allowlist.resolve().unwrap();
        assert!(first.allows("localhost:443"));
        // The same resolution, from the value and from a copy made before
        // it: no session resolves a destination again.
        assert!(Arc::ptr_eq(&first, &allowlist.resolve().unwrap()));
        let from_copy = std::thread::spawn(move || copy.resolve().unwrap())
            .join()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &from_copy));
        // A separate allowlist with the same text resolves on its own.
        let other = EgressAllowlist::new("internal:localhost:443")
            .resolve()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        // Nothing configured: an empty allowlist, which denies everything.
        assert!(EgressAllowlist::default().resolve().unwrap().is_empty());
        assert_eq!(EgressAllowlist::default().raw(), "");
        // A bad entry pins nothing; each try fails again.
        let bad = EgressAllowlist::new("*.example.com:443");
        assert!(bad.resolve().is_err());
        assert!(bad.resolve().is_err());
        // Debug shows what the operator wrote.
        let shown = format!("{allowlist:?}");
        assert!(shown.contains("raw: \"internal:localhost:443\""), "{shown}");
    }

    #[test]
    fn configured_destinations_lists_grants_then_bare_allowlist_entries() {
        let grants = vec![String::from("api.example.com:443")];
        assert_eq!(
            configured_destinations(
                &grants,
                " public:a.example:443, local:localhost:11434,local-keyless:localhost:8080 , b.example:80,internal:gw.corp:443"
            ),
            [
                "api.example.com:443",
                "a.example:443",
                "localhost:11434",
                "localhost:8080",
                "b.example:80",
                "gw.corp:443"
            ]
        );
        assert!(configured_destinations(&[], "").is_empty());
    }

    #[test]
    fn refuse_shared_egress_secrets_names_both_destinations_once_per_pair() {
        assert!(refuse_shared_egress_secrets(&[], "a.example:443, b.example:443").is_ok());
        assert!(
            refuse_shared_egress_secrets(&[String::from("a.example:443")], "a.example:443").is_ok()
        );
        let refused = refuse_shared_egress_secrets(&[], "x.y:1, z:2, x-y:1");
        assert!(
            matches!(
                &refused,
                Err(Error::InvalidConfig { key: "HATCHLING_EGRESS_ALLOWLIST", reason })
                    if reason.contains("\"x.y:1\" and \"x-y:1\"")
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn refuse_egress_secret_conflicts_runs_both_checks() {
        let fetch = |name: &str| VaultGrant {
            action: VaultAction::Fetch,
            name: name.parse().unwrap(),
        };
        assert!(refuse_egress_secret_conflicts(&[], &[], "a.example:443").is_ok());
        assert!(
            refuse_egress_secret_conflicts(&[fetch("egress-a-example-443")], &[], "a.example:443")
                .is_err()
        );
        assert!(refuse_egress_secret_conflicts(&[], &[], "a.example:443, a-example:443").is_err());
    }

    #[test]
    fn every_configured_destination_is_protected_by_the_lookup_mapping() {
        let refused = |vault: &str, grants: &str, allowlist: &str| {
            egress_overlap(vault, grants, allowlist).is_some()
        };
        // A destination named only as an egress grant, or only on the
        // allowlist, is protected either way.
        assert!(refused(
            "fetch:egress-api-example-com-443",
            "api.example.com:443",
            ""
        ));
        assert!(refused(
            "fetch:egress-api-example-com-443",
            "",
            "other.example.com:80, api.example.com:443"
        ));
        // Later entries are checked too, not only the first.
        assert!(refused(
            "fetch:app-token,remove:egress-b-example-com-80",
            "a.example.com:443,b.example.com:80",
            ""
        ));
        // The mapping is the broker's own, lossy one: a grant on the
        // shared name is refused for either destination that maps to it.
        assert_eq!(
            crate::serve::destination_to_secret_name("api-example:443"),
            crate::serve::destination_to_secret_name("api.example:443")
        );
        assert!(refused(
            "fetch:egress-api-example-443",
            "api-example:443",
            ""
        ));
        assert!(refused(
            "fetch:egress-api-example-443",
            "api.example:443",
            ""
        ));
    }

    #[test]
    fn an_egress_destination_without_an_overlapping_grant_still_configures() {
        // The legitimate path: vault grants on other names, egress
        // granted and allowlisted — parsing succeeds and keeps both.
        let mut vars = valid_vars();
        vars.insert(
            "HATCHLING_VAULT_GRANTS",
            "fetch:app-token,store:egress-note,fetch:egress-other-example-com-443",
        );
        vars.insert("HATCHLING_EGRESS_GRANTS", "api.example.com:443");
        vars.insert("HATCHLING_EGRESS_ALLOWLIST", "api.example.com:443");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.egress.grants, ["api.example.com:443"]);
        assert_eq!(config.egress.allowlist.raw(), "api.example.com:443");
        assert_eq!(config.vault.grants.len(), 3);
        // No egress configured at all: every vault grant stands.
        assert!(refuse_egress_secret_grants(&config.vault.grants, &[], "").is_ok());
    }

    fn ingress(raw: &str, channels: &[&str]) -> Result<Option<SocketPath>, Error> {
        let socket: SocketPath = "/run/hatchling/sock/broker.sock".parse().unwrap();
        let channels: Vec<String> = channels.iter().map(|c| (*c).to_owned()).collect();
        parse_channel_ingress(Some(raw), &socket, &channels)
    }

    fn ingress_refusal(raw: &str, channels: &[&str]) -> Option<String> {
        match ingress(raw, channels) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_CHANNEL_INGRESS",
                reason,
            }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    #[test]
    fn the_channel_ingress_parses_outside_the_cell_visible_directory() {
        for raw in [
            "/run/hatchling/ingress.sock",
            " /run/hatchling/ingress.sock ",
            "/run/hatchling/sockets/ingress.sock",
            "/tmp/ingress.sock",
        ] {
            assert_eq!(
                ingress(raw, &["local"])
                    .unwrap()
                    .map(|p| p.as_path().to_path_buf()),
                Some(PathBuf::from(raw.trim())),
                "{raw:?}"
            );
        }
        assert!(ingress("", &[]).unwrap().is_none());
        assert!(ingress("   ", &[]).unwrap().is_none());
        let socket: SocketPath = "/run/hatchling/sock/broker.sock".parse().unwrap();
        assert!(parse_channel_ingress(None, &socket, &[]).unwrap().is_none());
    }

    #[test]
    fn a_channel_ingress_the_cell_could_reach_is_refused() {
        for raw in [
            "/run/hatchling/sock/ingress.sock",
            "/run/hatchling/sock/broker.sock",
            "/run/hatchling/sock/deeper/ingress.sock",
        ] {
            let reason = ingress_refusal(raw, &["local"]);
            assert!(
                reason
                    .as_deref()
                    .is_some_and(|r| r.contains("which the cell mounts")),
                "{raw}: {reason:?}"
            );
        }
    }

    #[test]
    fn a_malformed_or_unanchored_channel_ingress_is_refused() {
        let reason = ingress_refusal("ingress.sock", &["local"]);
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("absolute path")),
            "{reason:?}"
        );
        let reason = ingress_refusal("/run/hatchling/ingress.sock", &["mail"]);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("not enabled")),
            "{reason:?}"
        );
    }

    #[test]
    fn config_carries_the_channel_ingress() {
        let mut vars = valid_vars();
        assert!(
            Config::parse(lookup(&vars))
                .unwrap()
                .channel_ingress
                .is_none()
        );
        vars.insert("HATCHLING_CHANNEL_INGRESS", "/tmp/hatchling-ingress.sock");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(
            config.channel_ingress.map(|p| p.as_path().to_path_buf()),
            Some(PathBuf::from("/tmp/hatchling-ingress.sock"))
        );
        vars.insert(
            "HATCHLING_CHANNEL_INGRESS",
            "/tmp/hatchling-test/ingress.sock",
        );
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::InvalidConfig {
                    key: "HATCHLING_CHANNEL_INGRESS",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn parses_valid_config_with_defaults() {
        let vars = valid_vars();
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(
            config.socket.as_path(),
            Path::new("/tmp/hatchling-test/broker.sock")
        );
        assert_eq!(config.agent_uid, PeerUid::from_kernel(1000));
        // The default is owner-only: fail-closed out of the box.
        assert_eq!(config.socket_mode, 0o600);
        assert_eq!(config.log_level, LogLevel::Info);
        assert_eq!(config.vault.root, Path::new("/tmp/hatchling-test/vault"));
        // Default key file is a dedicated 0700 directory sibling to the
        // vault root, never inside the store dir: a backup of the vault
        // must not capture the key, and the vault root's parent is
        // generally not owner-only.
        assert_eq!(
            config.vault.key_file,
            Path::new("/tmp/hatchling-test/vault.key/key")
        );
        assert_eq!(config.vault.grants.len(), 3);
    }

    #[test]
    fn explicit_key_file_is_honored() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_VAULT_KEY_FILE", "/etc/hatchling/vault.key");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.vault.key_file, Path::new("/etc/hatchling/vault.key"));
    }

    #[test]
    fn relative_key_file_is_an_error() {
        // Same rationale as the vault dir: a relative key path would
        // resolve against the broker's cwd.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_VAULT_KEY_FILE", "relative/vault.key");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(
            matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_VAULT_KEY_FILE"),
        );
    }

    #[test]
    fn missing_socket_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_BROKER_SOCKET");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(
            err,
            Error::MissingConfig("HATCHLING_BROKER_SOCKET")
        ));
    }

    #[test]
    fn missing_agent_uid_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_AGENT_UID");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_AGENT_UID")));
    }

    #[test]
    fn nobody_is_refused_as_the_agent_uid_unless_allowed() {
        let refused = |vars: &HashMap<&str, &str>| {
            matches!(
                Config::parse(lookup(vars)),
                Err(Error::InvalidConfig { key: "HATCHLING_AGENT_UID", reason })
                    if reason.contains("nobody") && reason.contains("HATCHLING_AGENT_ALLOW_NOBODY")
            )
        };
        let mut vars = valid_vars();
        vars.insert("HATCHLING_AGENT_UID", "65534");
        assert!(refused(&vars), "nobody, by default");
        for no in ["false", "0"] {
            vars.insert("HATCHLING_AGENT_ALLOW_NOBODY", no);
            assert!(refused(&vars), "{no}");
        }
        for yes in ["true", "1"] {
            vars.insert("HATCHLING_AGENT_ALLOW_NOBODY", yes);
            let config = Config::parse(lookup(&vars)).unwrap();
            assert_eq!(config.agent_uid, PeerUid::from_kernel(NOBODY_UID), "{yes}");
        }
        // A neighbour of nobody is just a uid.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_AGENT_UID", "65533");
        assert!(Config::parse(lookup(&vars)).is_ok());
        // The setting itself is strict, whatever the uid.
        vars.insert("HATCHLING_AGENT_ALLOW_NOBODY", "yes");
        assert!(matches!(
            Config::parse(lookup(&vars)),
            Err(Error::InvalidConfig {
                key: "HATCHLING_AGENT_ALLOW_NOBODY",
                ..
            })
        ));
    }

    #[test]
    fn the_agent_uid_helpers_refuse_only_an_unallowed_nobody() {
        let nobody = PeerUid::from_kernel(NOBODY_UID);
        assert!(refuse_shared_agent_uid(nobody, None).is_err());
        assert!(refuse_shared_agent_uid(nobody, Some("true")).is_ok());
        assert!(refuse_shared_agent_uid(PeerUid::from_kernel(1000), None).is_ok());
        let mut vars = valid_vars();
        let unused = || unreachable!("a given uid needs no lookup");
        assert_eq!(
            parse_agent_uid(&lookup(&vars), unused).unwrap(),
            PeerUid::from_kernel(1000)
        );
        vars.insert("HATCHLING_AGENT_UID", "65534");
        assert!(parse_agent_uid(&lookup(&vars), unused).is_err());
    }

    /// WO-014: on subordinate ids the agent's uid can be left out, and is
    /// then its cells' uid; on the broker's own map it can't.
    #[test]
    fn the_agent_uid_is_the_cells_subordinate_uid_when_left_out() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_AGENT_UID");
        assert!(matches!(
            parse_agent_uid(&lookup(&vars), || Ok(100_000)),
            Err(Error::MissingConfig("HATCHLING_AGENT_UID"))
        ));
        vars.insert("HATCHLING_CELL_IDS", "subordinate");
        assert_eq!(
            parse_agent_uid(&lookup(&vars), || Ok(100_000)).unwrap(),
            PeerUid::from_kernel(100_000)
        );
        // No range: the lookup's refusal, not a guessed uid.
        let refused = parse_agent_uid(&lookup(&vars), || {
            Err(Error::InvalidConfig {
                key: "HATCHLING_CELL_IDS",
                reason: String::from("no range in /etc/subuid"),
            })
        })
        .unwrap_err();
        assert!(refused.to_string().contains("/etc/subuid"), "{refused}");
        // A uid given is still the one used, and still checked.
        vars.insert("HATCHLING_AGENT_UID", "100001");
        assert_eq!(
            parse_agent_uid(&lookup(&vars), || Ok(100_000)).unwrap(),
            PeerUid::from_kernel(100_001)
        );
        vars.insert("HATCHLING_CELL_IDS", "sideways");
        vars.remove("HATCHLING_AGENT_UID");
        assert!(parse_agent_uid(&lookup(&vars), || Ok(100_000)).is_err());
    }

    #[test]
    fn invalid_agent_uid_is_an_error() {
        for raw in ["not-a-uid", "-1", ""] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_AGENT_UID", raw);
            let err = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_AGENT_UID"),
                "raw value {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn relative_socket_path_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_BROKER_SOCKET", "relative/broker.sock");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(
            matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_BROKER_SOCKET"),
        );
    }

    #[test]
    fn socket_mode_parses_octal() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_BROKER_SOCKET_MODE", "640");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.socket_mode, 0o640);
    }

    fn cap_refusal(raw: &str) -> Option<String> {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_BROKER_MAX_CONNECTIONS", raw);
        match Config::parse(lookup(&vars)) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_BROKER_MAX_CONNECTIONS",
                reason,
            }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    #[test]
    fn a_missing_connection_cap_is_the_documented_default() {
        let config = Config::parse(lookup(&valid_vars())).unwrap();
        assert_eq!(config.max_connections, DEFAULT_MAX_CONNECTIONS);
        assert_eq!(DEFAULT_MAX_CONNECTIONS, 16);
    }

    #[test]
    fn the_connection_cap_parses_within_its_bounds() {
        for (raw, cap) in [
            ("1", 1),
            ("3", 3),
            (" 64 ", 64),
            ("1024", MAX_MAX_CONNECTIONS),
        ] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_BROKER_MAX_CONNECTIONS", raw);
            assert_eq!(
                Config::parse(lookup(&vars)).unwrap().max_connections,
                cap,
                "{raw:?}"
            );
        }
        assert_eq!(MAX_MAX_CONNECTIONS, 1024);
    }

    #[test]
    fn an_invalid_connection_cap_fails_startup() {
        for (raw, why) in [
            ("0", "zero"),
            ("1025", "exceeds"),
            ("-1", "base-10"),
            ("many", "base-10"),
            ("", "base-10"),
            ("1.5", "base-10"),
        ] {
            let reason = cap_refusal(raw);
            assert!(
                reason.as_deref().is_some_and(|r| r.contains(why)),
                "{raw:?}: {reason:?}"
            );
        }
    }

    #[test]
    fn action_names_are_the_config_tokens_they_parse_from() {
        for action in [
            GoalsAction::GoalCreate,
            GoalsAction::GoalUpdate,
            GoalsAction::GoalClose,
            GoalsAction::TrackingAppend,
            GoalsAction::QueryGoals,
            GoalsAction::QueryTracking,
        ] {
            let parsed = parse_goals(
                Some(String::from("/srv/goals")),
                Some(action.name().to_owned()),
            )
            .unwrap();
            assert_eq!(parsed.grants, [action], "{}", action.name());
            assert_eq!(GoalsAction::from_name(action.name()), Some(action));
        }
        assert_eq!(GoalsAction::from_name("goal-delete"), None);
        assert_eq!(MemoryAction::from_name("erase"), None);
        for action in [
            MemoryAction::Write,
            MemoryAction::Recall,
            MemoryAction::Remove,
            MemoryAction::ContextAppend,
            MemoryAction::ContextRead,
        ] {
            let parsed = parse_memory(
                Some(String::from("/srv/memory")),
                Some(format!("user:{}", action.name())),
            )
            .unwrap();
            assert_eq!(
                parsed.grants.first().map(|grant| grant.action),
                Some(action),
                "{}",
                action.name()
            );
            assert_eq!(MemoryAction::from_name(action.name()), Some(action));
        }
    }

    fn scheduler(dir: &str) -> Result<SchedulerConfig, Error> {
        parse_scheduler(
            Some(dir),
            (None, None),
            Path::new("/srv/hatchling/vault"),
            Some(Path::new("/run/hatchling/sock")),
        )
    }

    #[test]
    fn scheduled_runs_get_a_run_root_and_model_apart_from_the_cell_mounts() {
        let config = scheduler("/srv/hatchling/schedules").unwrap();
        assert_eq!(
            config.run_root(),
            PathBuf::from("/srv/hatchling/schedules.runs")
        );
        assert_eq!((config.model_endpoint, config.model_name), (None, None));
        for (dir, why) in [
            ("/run/hatchling/sock/schedules", "socket directory"),
            ("/run/hatchling/sock", "socket directory"),
        ] {
            let err = scheduler(dir).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidConfig { key: "HATCHLING_SCHEDULER_DIR", reason }
                    if reason.contains(why)),
                "{dir}: {err:?}"
            );
        }
        // `/srv/hatchling/vault` with `.runs` is `/srv/hatchling/vault.runs`,
        // a sibling: only the store root itself may not sit on the vault.
        let err = parse_scheduler(
            Some("/srv/v/runs-parent/s"),
            (None, None),
            Path::new("/srv/v/runs-parent/s.runs"),
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("overlaps"), "{err:?}");
        let with_model = parse_scheduler(
            Some("/srv/s"),
            (
                Some(String::from("https://models.example.com/v1")),
                Some(String::from("m-1")),
            ),
            Path::new("/srv/v"),
            None,
        )
        .unwrap();
        assert_eq!(
            (
                with_model.model_endpoint.as_deref(),
                with_model.model_name.as_deref()
            ),
            (Some("https://models.example.com/v1"), Some("m-1"))
        );
        // Runs bind no socket, so a long scheduler dir is fine: past the
        // unix socket limit that runs once had to fit.
        let long = format!("/{}", "d".repeat(MAX_SOCKET_PATH_BYTES));
        assert!(parse_scheduler(Some(&long), (None, None), Path::new("/v"), None).is_ok());
        let blank = parse_scheduler(
            Some("/srv/s"),
            (Some(String::from("  ")), Some(String::new())),
            Path::new("/srv/v"),
            None,
        )
        .unwrap();
        assert_eq!((blank.model_endpoint, blank.model_name), (None, None));
        let mut vars = valid_vars();
        vars.insert("HATCHLING_SCHEDULER_DIR", "/tmp/hatchling-schedules/store");
        vars.insert("HATCHLING_SCHEDULER_MODEL_NAME", "m-2");
        let parsed = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(parsed.scheduler.model_name.as_deref(), Some("m-2"));
        assert_eq!(parsed.scheduler.agent_bin, None);
        vars.insert("HATCHLING_AGENT_BIN", "/usr/lib/hatchling/hatchling-agent");
        assert_eq!(
            Config::parse(lookup(&vars)).unwrap().scheduler.agent_bin,
            Some(PathBuf::from("/usr/lib/hatchling/hatchling-agent"))
        );
        vars.insert("HATCHLING_AGENT_BIN", "  ");
        assert_eq!(
            Config::parse(lookup(&vars)).unwrap().scheduler.agent_bin,
            None
        );
        vars.remove("HATCHLING_AGENT_BIN");
        vars.insert("HATCHLING_SCHEDULER_DIR", "/tmp/hatchling-test/runs");
        // `/tmp/hatchling-test` is the broker socket's directory here.
        assert!(Config::parse(lookup(&vars)).is_err());
    }

    #[test]
    fn the_scheduler_dir_is_absolute_and_apart_from_the_vault() {
        assert!(!scheduler("").unwrap().enabled());
        assert!(!scheduler("  ").unwrap().enabled());
        assert!(
            !parse_scheduler(None, (None, None), Path::new("/v"), None)
                .unwrap()
                .enabled()
        );
        let ok = scheduler("/srv/hatchling/schedules").unwrap();
        assert!(ok.enabled());
        assert_eq!(ok.root, PathBuf::from("/srv/hatchling/schedules"));
        assert!(!SchedulerConfig::disabled().enabled());
        for (dir, why) in [
            ("schedules", "absolute"),
            ("/srv/hatchling/vault", "overlaps"),
            ("/srv/hatchling/vault/schedules", "overlaps"),
            ("/srv/hatchling", "overlaps"),
        ] {
            let err = scheduler(dir).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidConfig { key: "HATCHLING_SCHEDULER_DIR", reason }
                    if reason.contains(why)),
                "{dir}: {err:?}"
            );
        }
        let mut vars = valid_vars();
        // Outside the broker socket's directory (`/tmp/hatchling-test`).
        vars.insert("HATCHLING_SCHEDULER_DIR", "/tmp/hatchling-schedules/store");
        assert!(Config::parse(lookup(&vars)).unwrap().scheduler.enabled());
        assert!(
            !Config::parse(lookup(&valid_vars()))
                .unwrap()
                .scheduler
                .enabled()
        );
    }

    #[test]
    fn socket_mode_accepts_boundary_777() {
        // Boundary: 0o777 is the largest valid mode. The literal pins
        // the `>` in the range check: a `>=` mutant would reject it.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_BROKER_SOCKET_MODE", "777");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.socket_mode, 0o777);
    }

    #[test]
    fn socket_mode_rejects_garbage_and_overflow() {
        for raw in ["", "not-octal", "999", "1000", "-1"] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_BROKER_SOCKET_MODE", raw);
            let err = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_BROKER_SOCKET_MODE"),
                "raw value {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn unknown_log_level_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_LOG_LEVEL", "verbose");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_LOG_LEVEL"),);
    }

    #[test]
    fn missing_vault_dir_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_VAULT_DIR");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_VAULT_DIR")));
    }

    #[test]
    fn relative_vault_dir_is_an_error() {
        // A relative vault dir would resolve against the broker's cwd:
        // a compromised working directory must not redirect the store.
        let mut vars = valid_vars();
        vars.insert("HATCHLING_VAULT_DIR", "relative/vault");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_VAULT_DIR"),);
    }

    #[test]
    fn missing_grants_mean_deny_everything() {
        // Deny-by-default: no grant config is not an error, it is an
        // empty grant set. Every vault op will be denied.
        let mut vars = valid_vars();
        vars.remove("HATCHLING_VAULT_GRANTS");
        let config = Config::parse(lookup(&vars)).unwrap();
        assert!(config.vault.grants.is_empty());
    }

    #[test]
    fn no_vault_grant_may_name_a_harness_vendor_key() {
        let grant = |action, name: &str| VaultGrant {
            action,
            name: name.parse().unwrap(),
        };
        assert_eq!(refuse_harness_secret_grants(Vec::new()).unwrap(), []);
        let others = vec![grant(VaultAction::Fetch, "harness-claude")];
        assert_eq!(
            refuse_harness_secret_grants(others.clone()).unwrap(),
            others
        );
        for (action, name, harness) in [
            (VaultAction::Fetch, "harness-claude-code", "claude-code"),
            (VaultAction::Store, "harness-codex-cli", "codex-cli"),
        ] {
            let refused = refuse_harness_secret_grants(vec![
                grant(VaultAction::Fetch, "app-token"),
                grant(action, name),
            ]);
            assert!(
                matches!(
                    &refused,
                    Err(Error::InvalidConfig { key: "HATCHLING_VAULT_GRANTS", reason })
                        if *reason == format!(
                            "grant {action}:{name} names the vendor key the broker holds \
                             for the {harness} harness"
                        )
                ),
                "{refused:?}"
            );
        }
        let record = refuse_harness_secret_grants(vec![grant(
            VaultAction::Fetch,
            crate::keeper::RELEASED_RECORD,
        )]);
        assert!(matches!(
            &record,
            Err(Error::InvalidConfig { key: "HATCHLING_VAULT_GRANTS", reason })
                if reason.contains("record of what it has released")
        ));
    }

    #[test]
    fn grants_parse_action_and_name_exactly() {
        // Grants are exact (action, name) pairs: no wildcards, no
        // prefixes. A fetch grant says nothing about store.
        let vars = valid_vars();
        let config = Config::parse(lookup(&vars)).unwrap();
        let grants = &config.vault.grants;
        assert!(grants.contains(&VaultGrant {
            action: VaultAction::Fetch,
            name: "app-token".parse().unwrap(),
        }));
        assert!(grants.contains(&VaultGrant {
            action: VaultAction::Store,
            name: "app-token".parse().unwrap(),
        }));
        assert!(!grants.contains(&VaultGrant {
            action: VaultAction::Remove,
            name: "app-token".parse().unwrap(),
        }));
    }

    #[test]
    fn malformed_grants_fail_closed() {
        // A typo'd grant must fail startup, never silently become a
        // missing (or extra) grant.
        for raw in [
            "fetch",            // no colon
            "list:app-token",   // unknown action
            "fetch:../escape",  // invalid name
            "fetch:",           // empty name
            ":app-token",       // empty action
            "fetch:app-token,", // trailing comma
            "fetch:app:token",  // name with colon
        ] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_VAULT_GRANTS", raw);
            let err = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_VAULT_GRANTS"),
                "grant {raw:?} should be rejected"
            );
        }
    }

    /// `parse_egress_grants` returns the parsed destinations verbatim.
    /// Pins the return-value substitution mutants (`Ok(vec![])`,
    /// `Ok(vec![String::new()])`, `Ok(vec!["xyzzy"])`).
    #[test]
    fn egress_grants_parse_verbatim() {
        // Missing or blank: no grants (deny-by-default).
        assert_eq!(parse_egress_grants(None).unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_egress_grants(Some(String::new())).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_egress_grants(Some(String::from("   "))).unwrap(),
            Vec::<String>::new()
        );
        // Valid grants come back exactly as given (trimmed).
        assert_eq!(
            parse_egress_grants(Some(String::from("a.example.com:443,b.example.com:80"))).unwrap(),
            vec![
                String::from("a.example.com:443"),
                String::from("b.example.com:80")
            ]
        );
        assert_eq!(
            parse_egress_grants(Some(String::from(
                "  a.example.com:443 , b.example.com:80  "
            )))
            .unwrap(),
            vec![
                String::from("a.example.com:443"),
                String::from("b.example.com:80")
            ]
        );
    }

    /// `parse_egress_grants` rejects entries without a `host:port`
    /// colon and empty entries. Pins the `!` deletion on the colon
    /// check (which would invert it).
    #[test]
    fn egress_grants_reject_malformed() {
        // No colon: rejected.
        assert!(parse_egress_grants(Some(String::from("no-colon-here"))).is_err());
        // Empty entry in the list: rejected.
        assert!(parse_egress_grants(Some(String::from("a.example.com:443,"))).is_err());
        assert!(parse_egress_grants(Some(String::from(",a.example.com:443"))).is_err());
    }

    /// `parse_channels` defaults to local, trims entries, and rejects
    /// empty entries fail-closed.
    #[test]
    fn channels_parse() {
        // Missing or blank: just local.
        assert_eq!(parse_channels(None).unwrap(), vec![String::from("local")]);
        assert_eq!(
            parse_channels(Some(String::from("  "))).unwrap(),
            vec![String::from("local")]
        );
        // Entries are trimmed.
        assert_eq!(
            parse_channels(Some(String::from("local, telegram"))).unwrap(),
            vec![String::from("local"), String::from("telegram")]
        );
        // Empty entries are rejected, not silently dropped.
        assert!(parse_channels(Some(String::from("local,"))).is_err());
        assert!(parse_channels(Some(String::from(",local"))).is_err());
        assert!(parse_channels(Some(String::from("local,,telegram"))).is_err());
    }
    #[test]
    fn thinking_mode_is_validated_even_when_the_scheduler_is_off() {
        use hatchling_core::ModelReasoning;
        for directory in [None, Some("/tmp/hatchling-schedules/store")] {
            let mut vars = valid_vars();
            if let Some(directory) = directory {
                vars.insert("HATCHLING_SCHEDULER_DIR", directory);
            }
            assert_eq!(
                Config::parse(lookup(&vars))
                    .unwrap()
                    .scheduler
                    .model_reasoning,
                ModelReasoning::ProviderDefault
            );
            assert_eq!(
                SchedulerConfig::disabled().model_reasoning,
                ModelReasoning::ProviderDefault
            );
            for (raw, expected) in [
                ("provider-default", ModelReasoning::ProviderDefault),
                ("disabled", ModelReasoning::Disabled),
            ] {
                vars.insert("HATCHLING_SCHEDULER_MODEL_REASONING", raw);
                let socket: SocketPath = "/tmp/hatchling-test/broker.sock".parse().unwrap();
                let (_, _, scheduler, _, _) =
                    parse_stores(&lookup(&vars), Path::new("/srv/vault"), &socket).unwrap();
                assert_eq!(scheduler.model_reasoning, expected);
                assert_eq!(
                    Config::parse(lookup(&vars))
                        .unwrap()
                        .scheduler
                        .model_reasoning,
                    expected
                );
            }
            for raw in ["", "off", "false", "Disabled", "disabled ", "auto"] {
                vars.insert("HATCHLING_SCHEDULER_MODEL_REASONING", raw);
                let error = Config::parse(lookup(&vars)).unwrap_err();
                assert!(
                    matches!(error, Error::InvalidConfig { key: "HATCHLING_SCHEDULER_MODEL_REASONING", reason } if reason == "expected provider-default or disabled")
                );
            }
        }
    }
}

#[cfg(test)]
mod skill_config_tests {
    use std::collections::HashSet;

    use hatchling_core::CapabilityName;

    use super::{SkillConfig, parse_skill_grants, parse_skills};
    use crate::Error;

    const ROOTS: &str = "/etc/hatchling/trust-roots";

    fn cap(raw: &str) -> CapabilityName {
        raw.parse().unwrap()
    }

    #[test]
    fn skill_grants_parse_as_typed_per_skill_sets() {
        let grants = parse_skill_grants(Some(String::from(
            " web:net.egress , web:fs.read,notes:fs.write ",
        )))
        .unwrap();
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants.get("web"),
            Some(&HashSet::from([cap("net.egress"), cap("fs.read")]))
        );
        assert_eq!(grants.get("notes"), Some(&HashSet::from([cap("fs.write")])));
        assert!(parse_skill_grants(None).unwrap().is_empty());
        assert!(
            parse_skill_grants(Some(String::from("  ")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_skill_grants_stop_startup() {
        // Empty, out-of-alphabet, and overlong capability names, an
        // empty skill name, a missing separator, an empty entry: each is
        // refused by name, never loaded as a grant that matches nothing.
        let long = format!("web:{}", "a".repeat(129));
        for raw in [
            "web:",
            "web:net egress",
            "web:fs/read",
            long.as_str(),
            ":fs.read",
            "web",
            "web:fs.read,,notes:fs.write",
        ] {
            assert!(
                matches!(
                    parse_skill_grants(Some(raw.to_owned())),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_SKILL_GRANTS",
                        ..
                    })
                ),
                "{raw:?} must be refused"
            );
        }
    }

    #[test]
    fn skills_are_enabled_only_by_a_configured_dir() {
        assert!(!SkillConfig::disabled().is_enabled());
        assert!(!parse_skills(None, None, None).unwrap().is_enabled());
        assert!(
            !parse_skills(Some(String::from(" ")), None, None)
                .unwrap()
                .is_enabled()
        );
        let config = parse_skills(
            Some(String::from("/srv/skills")),
            Some(String::from(ROOTS)),
            Some(String::from("web:net.egress")),
        )
        .unwrap();
        assert!(config.is_enabled());
        assert_eq!(
            config.grants.get("web"),
            Some(&HashSet::from([cap("net.egress")]))
        );
    }

    #[test]
    fn skills_dir_must_be_absolute() {
        assert!(matches!(
            parse_skills(
                Some(String::from("relative/skills")),
                Some(String::from(ROOTS)),
                None
            ),
            Err(Error::InvalidConfig {
                key: "HATCHLING_SKILLS_DIR",
                ..
            })
        ));
        let config = parse_skills(
            Some(String::from("/srv/skills")),
            Some(String::from(ROOTS)),
            None,
        )
        .unwrap();
        assert_eq!(config.skills_dir, std::path::PathBuf::from("/srv/skills"));
        assert_eq!(config.trust_roots_file, std::path::PathBuf::from(ROOTS));
    }

    #[test]
    fn the_trust_roots_file_is_required_and_absolute() {
        assert!(matches!(
            parse_skills(Some(String::from("/srv/skills")), None, None),
            Err(Error::MissingConfig("HATCHLING_SKILL_TRUST_ROOTS"))
        ));
        assert!(matches!(
            parse_skills(
                Some(String::from("/srv/skills")),
                Some(String::from("relative/roots")),
                None
            ),
            Err(Error::InvalidConfig {
                key: "HATCHLING_SKILL_TRUST_ROOTS",
                ..
            })
        ));
        // Surrounding whitespace is trimmed, not part of the path.
        let config = parse_skills(
            Some(String::from("/srv/skills")),
            Some(format!(" {ROOTS} ")),
            None,
        )
        .unwrap();
        assert_eq!(config.trust_roots_file, std::path::PathBuf::from(ROOTS));
    }
}

#[cfg(test)]
mod goals_config_tests {
    use super::{GoalsAction, parse_goals};
    use crate::Error;

    #[test]
    fn a_missing_or_blank_dir_disables_the_store() {
        for dir in [None, Some(String::new()), Some(String::from("  "))] {
            let config = parse_goals(dir, Some(String::from("goal-create"))).unwrap();
            assert_eq!(config.root, std::path::PathBuf::new());
            assert!(config.grants.is_empty(), "a disabled store holds no grants");
        }
    }

    #[test]
    fn goals_dir_must_be_absolute() {
        assert!(matches!(
            parse_goals(Some(String::from("rel/goals")), None),
            Err(Error::InvalidConfig {
                key: "HATCHLING_GOALS_DIR",
                ..
            })
        ));
        let config = parse_goals(Some(String::from("/srv/goals")), None).unwrap();
        assert_eq!(config.root, std::path::PathBuf::from("/srv/goals"));
        assert!(config.grants.is_empty());
    }

    #[test]
    fn grants_accept_exactly_the_six_actions() {
        let config = parse_goals(
            Some(String::from("/srv/goals")),
            Some(String::from(
                " goal-create, goal-update,goal-close ,tracking-append,,query-goals,query-tracking",
            )),
        )
        .unwrap();
        assert_eq!(
            config.grants,
            vec![
                GoalsAction::GoalCreate,
                GoalsAction::GoalUpdate,
                GoalsAction::GoalClose,
                GoalsAction::TrackingAppend,
                GoalsAction::QueryGoals,
                GoalsAction::QueryTracking,
            ]
        );
        for bad in ["goal-delete", "query-memory", "user:write", "Goal-Create"] {
            assert!(
                matches!(
                    parse_goals(Some(String::from("/srv/goals")), Some(String::from(bad))),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_GOALS_GRANTS",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }
}

#[cfg(test)]
mod memory_config_tests {
    use super::parse_memory;
    use crate::Error;

    #[test]
    fn memory_dir_must_be_absolute() {
        assert!(matches!(
            parse_memory(Some(String::from("rel/mem")), None),
            Err(Error::InvalidConfig {
                key: "HATCHLING_MEMORY_DIR",
                ..
            })
        ));
        assert_eq!(
            parse_memory(Some(String::from("/srv/mem")), None)
                .unwrap()
                .root,
            std::path::PathBuf::from("/srv/mem")
        );
    }

    #[test]
    fn grants_accept_exactly_the_three_scopes() {
        for scope in ["user", "project", "session"] {
            let config = parse_memory(
                Some(String::from("/srv/mem")),
                Some(format!("{scope}:write")),
            )
            .unwrap();
            assert_eq!(
                config.grants.first().map(|g| g.scope.as_str()),
                Some(scope),
                "{scope}"
            );
        }
        assert!(matches!(
            parse_memory(
                Some(String::from("/srv/mem")),
                Some(String::from("global:write"))
            ),
            Err(Error::InvalidConfig {
                key: "HATCHLING_MEMORY_GRANTS",
                ..
            })
        ));
    }
}

#[cfg(test)]
mod web_config_tests {
    use std::collections::HashMap;

    use super::{Config, SearchLog, WebConfig};
    use crate::Error;

    /// The web settings from `web` and `refuse`.
    fn parse_web(web: Option<&str>, refuse: Option<&str>) -> Result<WebConfig, Error> {
        super::parse_web(&|key: &str| match key {
            "HATCHLING_WEB" => web.map(str::to_owned),
            "HATCHLING_WEB_REFUSE" => refuse.map(str::to_owned),
            _ => None,
        })
    }

    /// The search settings from `search` and `log`.
    fn parse_search(search: Option<&str>, log: Option<&str>) -> Result<WebConfig, Error> {
        super::parse_web(&|key: &str| match key {
            "HATCHLING_SEARCH" => search.map(str::to_owned),
            "HATCHLING_SEARCH_LOG" => log.map(str::to_owned),
            _ => None,
        })
    }

    /// The allowed sites' store is off unless the operator gives it an
    /// absolute directory.
    #[test]
    fn the_allowed_sites_store_is_an_absolute_directory_or_off() {
        let parse = |raw: Option<&str>| {
            super::parse_web(&|key: &str| match key {
                "HATCHLING_WEB_GRANTS_DIR" => raw.map(str::to_owned),
                _ => None,
            })
        };
        for off in [None, Some(""), Some("  ")] {
            assert!(parse(off).unwrap().grants.as_os_str().is_empty(), "{off:?}");
        }
        assert_eq!(
            parse(Some("/srv/hatchling/sites")).unwrap().grants,
            std::path::PathBuf::from("/srv/hatchling/sites")
        );
        assert!(matches!(
            parse(Some("relative/sites")),
            Err(Error::InvalidConfig {
                key: "HATCHLING_WEB_GRANTS_DIR",
                ..
            })
        ));
        assert!(WebConfig::off().grants.as_os_str().is_empty());
    }

    /// The signed-in profiles' store is off unless the operator gives it an
    /// absolute directory (WO-030).
    #[test]
    fn the_signed_in_profiles_store_is_an_absolute_directory_or_off() {
        let parse = |raw: Option<&str>| {
            super::parse_web(&|key: &str| match key {
                "HATCHLING_BROWSER_PROFILES_DIR" | "HATCHLING_BROWSER_CREDENTIALS_DIR" => {
                    raw.map(str::to_owned)
                }
                _ => None,
            })
        };
        for off in [None, Some(""), Some("  ")] {
            assert!(
                parse(off).unwrap().profiles.as_os_str().is_empty(),
                "{off:?}"
            );
        }
        assert_eq!(
            parse(Some("/srv/hatchling/profiles")).unwrap().profiles,
            std::path::PathBuf::from("/srv/hatchling/profiles")
        );
        assert_eq!(
            parse(Some("/srv/hatchling/profiles")).unwrap().credentials,
            std::path::PathBuf::from("/srv/hatchling/profiles"),
            "the sign-ins' store, set the same way"
        );
        assert!(parse(None).unwrap().credentials.as_os_str().is_empty());
        assert!(matches!(
            parse(Some("relative/profiles")),
            Err(Error::InvalidConfig {
                key: "HATCHLING_BROWSER_PROFILES_DIR",
                ..
            })
        ));
        assert!(WebConfig::off().profiles.as_os_str().is_empty());
        assert!(WebConfig::off().credentials.as_os_str().is_empty());
        // The parse itself: a setting's store, none, or refused.
        assert_eq!(
            super::store_dir("K", Some(" /a/b ")).unwrap(),
            std::path::PathBuf::from("/a/b")
        );
        assert!(super::store_dir("K", None).unwrap().as_os_str().is_empty());
        assert!(matches!(
            super::store_dir("K", Some("a/b")),
            Err(Error::InvalidConfig { key: "K", .. })
        ));
    }

    /// Search is off unless the operator chooses Brave; a query's text is
    /// logged only on the owner's opt-in; no grant may name the key.
    #[test]
    fn search_is_off_unless_chosen_and_its_text_logged_only_when_asked() {
        for off in [None, Some(""), Some("off")] {
            assert!(!parse_search(off, None).unwrap().search, "{off:?}");
        }
        assert!(parse_search(Some(" brave "), None).unwrap().search);
        for length in [None, Some(""), Some("length")] {
            assert!(
                parse_search(None, length).unwrap().search_log == SearchLog::Length,
                "{length:?}"
            );
        }
        assert_eq!(
            parse_search(None, Some("queries")).unwrap().search_log,
            SearchLog::Queries
        );
        assert!(!WebConfig::off().search);
        assert_eq!(WebConfig::off().search_log, SearchLog::Length);
        assert!(matches!(
            parse_search(Some("google"), None),
            Err(Error::InvalidConfig {
                key: "HATCHLING_SEARCH",
                ..
            })
        ));
        assert!(matches!(
            parse_search(None, Some("all")),
            Err(Error::InvalidConfig {
                key: "HATCHLING_SEARCH_LOG",
                ..
            })
        ));
        let grant = super::VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: crate::web_ops::SEARCH_KEY.parse().unwrap(),
        };
        let refused = super::refuse_harness_secret_grants(vec![grant]);
        assert!(matches!(
            &refused,
            Err(Error::InvalidConfig { key: "HATCHLING_VAULT_GRANTS", reason })
                if reason.contains("search provider's key")
        ));
    }

    /// The web is off unless the operator says `sites`; the refused hosts
    /// are names of lowercase ASCII labels, each refusing itself and every
    /// name under it (WO-028).
    #[test]
    fn the_web_is_off_unless_turned_on_and_refuses_what_the_operator_names() {
        for off in [None, Some(""), Some("off"), Some(" off ")] {
            let web = parse_web(off, None).unwrap();
            assert!(!web.enabled, "{off:?}");
        }
        let on = parse_web(
            Some("sites"),
            Some(" blocked.example, ,tracker.example.net "),
        )
        .unwrap();
        assert!(on.enabled);
        assert!(on.attended, "the broker's own session has its person there");
        assert_eq!(on.refused, ["blocked.example", "tracker.example.net"]);
        assert!(on.refuses("blocked.example"));
        assert!(on.refuses("a.b.blocked.example"));
        assert!(!on.refuses("notblocked.example"));
        assert!(!on.refuses("blocked.example.com"));
        assert!(!on.refuses("example"));
        assert!(!WebConfig::off().refuses("blocked.example"));
        assert!(!WebConfig::off().enabled);
        assert!(!WebConfig::off().attended);
        for bad in ["on", "all", "true"] {
            assert!(
                matches!(
                    parse_web(Some(bad), None),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_WEB",
                        ..
                    })
                ),
                "{bad}"
            );
        }
        for bad in [
            "Blocked.example",
            "a.example:443",
            "1.2.3.4",
            "a.example/x",
            "*.example",
            "user@a.example",
        ] {
            assert!(
                matches!(
                    parse_web(Some("sites"), Some(bad)),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_WEB_REFUSE",
                        ..
                    })
                ),
                "{bad}"
            );
        }
        let vars: HashMap<&str, &str> = HashMap::from([
            (
                "HATCHLING_BROKER_SOCKET",
                "/tmp/hatchling-web-config/broker.sock",
            ),
            ("HATCHLING_AGENT_UID", "4242"),
            ("HATCHLING_VAULT_DIR", "/srv/hatchling/vault"),
            ("HATCHLING_WEB", "sites"),
            ("HATCHLING_WEB_REFUSE", "blocked.example"),
        ]);
        let config = Config::parse(|key| vars.get(key).map(|value| (*value).to_owned())).unwrap();
        assert!(config.web.enabled);
        assert_eq!(config.web.refused, ["blocked.example"]);
    }
}
