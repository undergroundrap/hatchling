//! Who may call the local API, and with what.
//!
//! The API's adapter ([`crate::api`]) only transports: every decision
//! about a caller is made here, in the broker. A caller is identified by a
//! [`Provider`], the pluggable boundary: v0.1 ships [`PeerProvider`],
//! which admits the socket owner by its kernel peer credentials, and a
//! hosted deployment's token provider plugs in behind the same trait
//! without changing the adapter or the protocol.
//!
//! Whatever a provider says, a caller's grants are narrowed to the
//! owner's ([`admit`]), the same rule a subagent's are: the API
//! never gives a caller more than the human authorized.
//!
//! The settings are their own, apart from [`Config`]:
//!
//! - `HATCHLING_API_SOCKET`: the API's Unix socket, an absolute path
//!   outside the broker socket's directory. Unset means no API. Its audit
//!   trail is the activity log, so it needs `HATCHLING_SCHEDULER_DIR`.
//! - `HATCHLING_UI_SOCKET`: the UI's socket, where `hatchling-host`
//!   carries the browser's channel. The same rules as the API socket,
//!   and one more: every caller must present the UI token
//!   ([`TokenProvider`]), which the broker mints at each start
//!   ([`mint_ui_token`]) into `<socket>.token`, readable by its owner
//!   alone. A web page that can reach the host's port can't read it.
//! - `HATCHLING_API_GRANTS`: the local caller's grants, as grant labels
//!   (`goals:query-goals,channel-send:local`). Each must be one the owner holds.
//!   Unset means none: the caller can follow the activity, and do only
//!   what needs no grant.
//! - `HATCHLING_API_NETWORK`: a network listener's address. Never the
//!   default, and refused unless `HATCHLING_API_ALLOW_NETWORK` is `true`;
//!   even then refused in this build, which has no token provider to
//!   identify a network caller.

use std::collections::BTreeSet;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use subtle::ConstantTimeEq as _;

use hatchling_core::messages::ScheduleGrant;
use hatchling_core::subagent::{grant_label, parse_grant_label};
use hatchling_core::ui::ApiToken;
use hatchling_core::{PeerUid, SocketPath, peer_credentials};

use crate::environment as env;
use crate::schedule_ops::held_grants;
use crate::{Config, Error};

/// The API's own settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiConfig {
    /// The API socket, when the API is on.
    pub socket: Option<SocketPath>,
    /// The UI socket, when the UI is on: its callers must also present
    /// the UI token ([`TokenProvider`]).
    pub ui_socket: Option<SocketPath>,
    /// The local caller's grants: a subset of the owner's.
    pub grants: BTreeSet<ScheduleGrant>,
}

impl ApiConfig {
    /// The settings from the environment, checked against `config`.
    ///
    /// # Errors
    ///
    /// As [`ApiConfig::parse`].
    pub fn from_env(config: &Config) -> Result<Self, Error> {
        Self::parse(config, |key| std::env::var(key).ok())
    }

    /// The settings `get` reads, checked against `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a socket that is not an
    /// absolute path, lies inside the broker socket's directory, or has
    /// no activity log to audit to; for a grant label that names no
    /// grant or a grant the owner does not hold; and for any network
    /// listener.
    pub fn parse(config: &Config, get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        refuse_network(
            get(env::HATCHLING_API_NETWORK.name).as_deref(),
            get(env::HATCHLING_API_ALLOW_NETWORK.name).as_deref(),
        )?;
        let grants = parse_grants(
            get(env::HATCHLING_API_GRANTS.name).as_deref(),
            &held_grants(config),
        )?;
        let socket = parse_socket(
            get(env::HATCHLING_API_SOCKET.name).as_deref(),
            config,
            "HATCHLING_API_SOCKET",
        )?;
        let ui_socket = parse_socket(
            get(env::HATCHLING_UI_SOCKET.name).as_deref(),
            config,
            "HATCHLING_UI_SOCKET",
        )?;
        if ui_socket.is_some() && ui_socket == socket {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_UI_SOCKET",
                reason: String::from(
                    "the UI socket must differ from the API socket: each holds its callers to its own rule",
                ),
            });
        }
        Ok(Self {
            socket,
            ui_socket,
            grants,
        })
    }
}

/// A strict boolean setting: `true`/`1` or `false`/`0`, unset meaning
/// false.
pub(crate) fn flag(raw: Option<&str>, key: &'static str) -> Result<bool, Error> {
    match raw.map(str::trim) {
        None | Some("false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some(other) => Err(Error::InvalidConfig {
            key,
            reason: format!("must be true, 1, false or 0, not {other:?}"),
        }),
    }
}

/// Refuses any network listener: never the default, and in this build
/// never possible, since only a token provider could identify a caller
/// over a network and none ships.
fn refuse_network(network: Option<&str>, allow: Option<&str>) -> Result<(), Error> {
    let allowed = flag(allow, "HATCHLING_API_ALLOW_NETWORK")?;
    let Some(address) = network.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(());
    };
    if !allowed {
        return Err(Error::InvalidConfig {
            key: "HATCHLING_API_NETWORK",
            reason: format!(
                "a network listener is never the default: the API listens on its local socket \
                 only. To ask for one at {address:?}, set HATCHLING_API_ALLOW_NETWORK=true"
            ),
        });
    }
    tracing::warn!(
        address,
        "a network listener for the API was asked for; it is refused in this build"
    );
    Err(Error::InvalidConfig {
        key: "HATCHLING_API_NETWORK",
        reason: format!(
            "a network listener at {address:?} needs a token provider to identify its callers, \
             and this build ships only the local socket's peer provider: nothing will listen there"
        ),
    })
}

/// The local caller's grants: labels, each one the owner holds.
fn parse_grants(
    raw: Option<&str>,
    held: &BTreeSet<ScheduleGrant>,
) -> Result<BTreeSet<ScheduleGrant>, Error> {
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_API_GRANTS",
        reason,
    };
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(|label| {
            let grant = parse_grant_label(label).ok_or_else(|| invalid(format!("{label:?} names no grant")))?;
            if held.contains(&grant) {
                Ok(grant)
            } else {
                Err(invalid(format!(
                    "{label:?} is not a grant the owner holds: an API caller gets no more than the owner"
                )))
            }
        })
        .collect()
}

/// An API or UI socket, the setting `key`: absolute, outside the cell's
/// sight, with an activity log to audit to.
fn parse_socket(
    raw: Option<&str>,
    config: &Config,
    key: &'static str,
) -> Result<Option<SocketPath>, Error> {
    let socket = parse_socket_path(raw, config, key)?;
    if socket.is_some() && !config.scheduler.enabled() {
        return Err(Error::InvalidConfig {
            key,
            reason: String::from(
                "every API call is recorded in the activity log, which needs HATCHLING_SCHEDULER_DIR",
            ),
        });
    }
    Ok(socket)
}

/// A local socket's path setting: unset or blank is none; otherwise an
/// absolute path outside the broker socket's directory, which cells
/// mount.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] under `key` for any other path.
pub(crate) fn parse_socket_path(
    raw: Option<&str>,
    config: &Config,
    key: &'static str,
) -> Result<Option<SocketPath>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let invalid = |reason: String| Error::InvalidConfig { key, reason };
    let socket: SocketPath = raw
        .parse()
        .map_err(|_| invalid(format!("the socket must be an absolute path: {raw:?}")))?;
    if let Some(cell_visible) = config.socket.as_path().parent() {
        if socket.as_path().starts_with(cell_visible) {
            return Err(invalid(format!(
                "the socket {raw:?} is inside the broker socket directory {}, which the cell mounts",
                cell_visible.display()
            )));
        }
    }
    Ok(Some(socket))
}

/// An identified API caller and its grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    label: String,
    grants: BTreeSet<ScheduleGrant>,
}

impl Caller {
    /// A caller named `label` holding `grants`.
    #[must_use]
    pub const fn new(label: String, grants: BTreeSet<ScheduleGrant>) -> Self {
        Self { label, grants }
    }

    /// How the activity feed names the caller.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The caller's grants.
    #[must_use]
    pub const fn grants(&self) -> &BTreeSet<ScheduleGrant> {
        &self.grants
    }

    /// The caller's grants as labels, for the activity feed.
    #[must_use]
    pub fn grant_labels(&self) -> Vec<String> {
        self.grants.iter().map(grant_label).collect()
    }

    /// Whether the caller holds every grant in `needed`.
    #[must_use]
    pub fn holds(&self, needed: &BTreeSet<ScheduleGrant>) -> bool {
        needed.is_subset(&self.grants)
    }
}

/// Identifies API callers: the pluggable boundary between the API and
/// how a deployment knows who is calling.
pub trait Provider: Send + Sync {
    /// Who is calling on `stream`, given the token its `Hello`
    /// presented, or `None` for a caller this provider does not admit.
    fn identify(&self, stream: &UnixStream, token: Option<&ApiToken>) -> Option<Caller>;
}

/// Whether the peer on `stream` is `owner`.
///
/// The API's check, by the kernel's peer credentials, before it reads a
/// byte. A peer that is not the owner, or whose credentials cannot be
/// read, is refused and the refusal logged.
#[must_use]
pub fn from_owner(stream: &UnixStream, owner: PeerUid) -> bool {
    match peer_credentials(stream) {
        Ok(peer) if peer.uid() == owner => true,
        Ok(peer) => {
            tracing::warn!(
                uid = %peer.uid(),
                owner = %owner,
                "API peer is not the socket's owner: closed before a byte"
            );
            false
        }
        Err(error) => {
            tracing::warn!(%error, "API peer's credentials unreadable: closed before a byte");
            false
        }
    }
}

/// The local provider: admits exactly the socket's owner, by the kernel's
/// peer credentials, with the configured grants. A token is not its
/// concern and is ignored.
#[derive(Debug, Clone)]
pub struct PeerProvider {
    owner: PeerUid,
    grants: BTreeSet<ScheduleGrant>,
}

impl PeerProvider {
    /// Admits `owner`, granting `grants`.
    #[must_use]
    pub const fn new(owner: PeerUid, grants: BTreeSet<ScheduleGrant>) -> Self {
        Self { owner, grants }
    }
}

impl Provider for PeerProvider {
    fn identify(&self, stream: &UnixStream, _token: Option<&ApiToken>) -> Option<Caller> {
        let peer = match peer_credentials(stream) {
            Ok(peer) => peer,
            Err(error) => {
                tracing::warn!(%error, "API caller's peer credentials unreadable: not admitted");
                return None;
            }
        };
        if peer.uid() != self.owner {
            tracing::warn!(
                uid = %peer.uid(),
                owner = %self.owner,
                "API caller is not the socket's owner: not admitted"
            );
            return None;
        }
        Some(Caller::new(
            format!("uid {}", peer.uid()),
            self.grants.clone(),
        ))
    }
}

/// The UI socket's provider: the socket's owner, as [`PeerProvider`]
/// admits it, presenting the UI token the broker minted at this start.
/// A caller with no token, or another, is not admitted.
#[derive(Clone)]
pub struct TokenProvider {
    peer: PeerProvider,
    token: ApiToken,
}

impl std::fmt::Debug for TokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenProvider")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl TokenProvider {
    /// Admits whom `peer` admits, when it presents `token`.
    #[must_use]
    pub const fn new(peer: PeerProvider, token: ApiToken) -> Self {
        Self { peer, token }
    }
}

impl Provider for TokenProvider {
    fn identify(&self, stream: &UnixStream, token: Option<&ApiToken>) -> Option<Caller> {
        // Compared in constant time: how long the check takes says
        // nothing about how much of a guess was right.
        let presented = token.map(ApiToken::expose).unwrap_or_default();
        if !bool::from(presented.as_bytes().ct_eq(self.token.expose().as_bytes())) {
            tracing::warn!("UI caller presented no token, or the wrong one: not admitted");
            return None;
        }
        let caller = self.peer.identify(stream, None)?;
        Some(Caller::new(format!("ui, {}", caller.label), caller.grants))
    }
}

/// Where the UI token for `socket` is kept: the socket's path with
/// `.token` added.
#[must_use]
pub fn ui_token_path(socket: &SocketPath) -> PathBuf {
    let mut path = socket.as_path().as_os_str().to_owned();
    path.push(".token");
    PathBuf::from(path)
}

/// Mints a fresh UI token, and keeps it at `path` for its owner alone.
///
/// A token from an earlier start is removed first, and only when it's a
/// regular file of the broker's user: anything else there is refused,
/// never removed. The new file is then created exclusively with mode
/// 0600, so anything planted at `path` in between, a symlink included,
/// fails the call rather than receiving the token.
///
/// # Errors
///
/// [`Error::SocketSetup`] if the old token can't be removed or the new
/// one can't be made or written.
pub fn mint_ui_token(path: &Path) -> Result<ApiToken, Error> {
    mint_ui_token_as(path, hatchling_sys::euid())
}

/// Whether `metadata` is a regular file's, by its file type.
fn file_by_type(metadata: &Metadata) -> bool {
    metadata.file_type().is_file()
}

/// The file-type bits of a raw mode (`S_IFMT`).
const MODE_TYPE: u32 = 0o170_000;

/// A regular file's file-type bits (`S_IFREG`).
const MODE_FILE: u32 = 0o100_000;

/// Whether `metadata` is a regular file's, by its raw mode bits: the same
/// fact as [`file_by_type`], read through other code.
fn file_by_mode(metadata: &Metadata) -> bool {
    metadata.mode() & MODE_TYPE == MODE_FILE
}

/// Removes an earlier start's token at `path`, when it's a regular file
/// `owner` owns; nothing there is fine. Each check is its own, so no one
/// changed operator removes anything else.
fn clear_old_token(path: &Path, owner: u32) -> Result<(), Error> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(Error::SocketSetup { source }),
    };
    let refused = || Error::TokenPathNotOurs {
        path: path.to_path_buf(),
    };
    if !file_by_type(&metadata) {
        return Err(refused());
    }
    if !file_by_mode(&metadata) {
        return Err(refused());
    }
    if metadata.uid() != owner {
        return Err(refused());
    }
    std::fs::remove_file(path).map_err(|source| Error::SocketSetup { source })
}

/// [`mint_ui_token`], with `owner` the uid an earlier token must have.
fn mint_ui_token_as(path: &Path, owner: u32) -> Result<ApiToken, Error> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let setup = |source| Error::SocketSetup { source };
    let token = crate::approval::random_hex("").map_err(setup)?;
    clear_old_token(path, owner)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(setup)?;
    file.write_all(token.as_bytes()).map_err(setup)?;
    Ok(ApiToken::new(token))
}

/// The caller `provider` identifies on `stream`, its grants narrowed to
/// `held`, the owner's: whatever a provider claims, a caller never holds
/// more than the owner.
#[must_use]
pub fn admit(
    provider: &dyn Provider,
    held: &BTreeSet<ScheduleGrant>,
    stream: &UnixStream,
    token: Option<&ApiToken>,
) -> Option<Caller> {
    let caller = provider.identify(stream, token)?;
    for widened in caller.grants.difference(held) {
        tracing::warn!(
            caller = caller.label,
            grant = %grant_label(widened),
            "API provider claimed a grant the owner does not hold: dropped"
        );
    }
    let grants = caller.grants.intersection(held).cloned().collect();
    Some(Caller::new(caller.label, grants))
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: the API modules' tests share these helpers"
)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A scratch directory, owner-only, removed when the guard drops.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh scratch directory for the test `name`.
    pub(crate) fn scratch(name: &str) -> Scratch {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("hatchling-api-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Scratch(dir)
    }

    pub(crate) fn real_uid() -> PeerUid {
        PeerUid::from_kernel(rustix::process::getuid().as_raw())
    }

    fn goals(action: &str) -> ScheduleGrant {
        ScheduleGrant::Goals {
            action: action.to_owned(),
        }
    }

    /// A config whose owner holds `goals:query-goals` and sends on
    /// `local`, with scheduling on at `scheduler` when it is set. The
    /// agent is another uid, as in any real deployment.
    pub(crate) fn owner_config(scheduler: Option<&str>) -> Config {
        let root = PathBuf::from("/nonexistent/hatchling-api-vault");
        let mut config = Config {
            socket: "/tmp/hatchling-test-api-dummy/broker.sock".parse().unwrap(),
            agent_uid: PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1)),
            socket_mode: 0o600,
            max_connections: crate::DEFAULT_MAX_CONNECTIONS,
            scheduler: crate::SchedulerConfig::disabled(),
            log_level: hatchling_core::LogLevel::Error,
            vault: crate::VaultConfig {
                key_file: crate::VaultConfig::default_key_file(&root),
                root,
                grants: Vec::new(),
            },
            egress: crate::EgressConfig {
                allowlist: crate::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: vec![String::from("local")],
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: crate::SkillConfig::disabled(),
            memory: crate::MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: crate::ArtifactConfig::disabled(),
            spaces: crate::SpaceConfig::disabled(),
            web: crate::WebConfig::off(),
            goals: crate::GoalsConfig::disabled(),
        };
        config.goals.grants = vec![crate::GoalsAction::QueryGoals];
        if let Some(root) = scheduler {
            config.scheduler.root = PathBuf::from(root);
        }
        config
    }

    fn parse(config: &Config, vars: &[(&str, &str)]) -> Result<ApiConfig, Error> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        ApiConfig::parse(config, |key| vars.get(key).cloned())
    }

    fn reason(result: Result<ApiConfig, Error>, key: &str) -> String {
        match result {
            Err(Error::InvalidConfig { key: got, reason }) if got == key => reason,
            other => format!("not a {key} refusal: {other:?}"),
        }
    }

    #[test]
    fn the_ui_socket_is_held_to_the_api_sockets_rules_and_apart_from_it() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        let on = parse(
            &config,
            &[
                ("HATCHLING_API_SOCKET", "/run/user/1000/hatchling-api.sock"),
                ("HATCHLING_UI_SOCKET", "/run/user/1000/hatchling-ui.sock"),
            ],
        )
        .unwrap();
        assert_eq!(
            on.ui_socket,
            Some("/run/user/1000/hatchling-ui.sock".parse().unwrap())
        );
        // The same socket for both: refused.
        let same = parse(
            &config,
            &[
                ("HATCHLING_API_SOCKET", "/run/user/1000/hatchling.sock"),
                ("HATCHLING_UI_SOCKET", "/run/user/1000/hatchling.sock"),
            ],
        );
        assert!(reason(same, "HATCHLING_UI_SOCKET").contains("must differ"));
        // Inside the cell's sight, or relative: refused, by its own name.
        let inside = parse(
            &config,
            &[(
                "HATCHLING_UI_SOCKET",
                "/tmp/hatchling-test-api-dummy/ui.sock",
            )],
        );
        assert!(reason(inside, "HATCHLING_UI_SOCKET").contains("which the cell mounts"));
        let relative = parse(&config, &[("HATCHLING_UI_SOCKET", "ui.sock")]);
        assert!(reason(relative, "HATCHLING_UI_SOCKET").contains("absolute path"));
        // With no activity log to audit to: refused.
        let unaudited = parse(
            &owner_config(None),
            &[("HATCHLING_UI_SOCKET", "/run/user/1000/hatchling-ui.sock")],
        );
        assert!(reason(unaudited, "HATCHLING_UI_SOCKET").contains("activity log"));
    }

    #[test]
    fn the_ui_admits_only_its_owner_with_its_token() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let grants = BTreeSet::from([goals("query-goals")]);
        let token = ApiToken::new(String::from("0123456789abcdef"));
        let ui = TokenProvider::new(PeerProvider::new(real_uid(), grants.clone()), token.clone());
        let caller = ui.identify(&stream, Some(&token)).unwrap();
        assert_eq!(caller.label(), format!("ui, uid {}", real_uid()));
        assert_eq!(caller.grants(), &grants);
        for wrong in [
            None,
            Some(ApiToken::new(String::new())),
            Some(ApiToken::new(String::from("0123456789abcdee"))),
            Some(ApiToken::new(String::from("0123456789abcdef0"))),
        ] {
            assert!(ui.identify(&stream, wrong.as_ref()).is_none(), "{wrong:?}");
        }
        // The token alone isn't enough: the owner's credentials still are.
        let stranger = PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1));
        let theirs = TokenProvider::new(PeerProvider::new(stranger, grants), token.clone());
        assert!(theirs.identify(&stream, Some(&token)).is_none());
        // Bound first: a format string inside `assert!`'s condition trips
        // clippy 1.85's literal_string_with_formatting_args, which panics.
        let shown = format!("{ui:?}");
        assert!(shown.starts_with("TokenProvider"), "{shown}");
        assert!(!shown.contains("0123456789abcdef"));
    }

    #[test]
    fn only_a_regular_file_of_the_brokers_user_is_an_old_token() {
        use std::os::unix::fs::{MetadataExt as _, symlink};
        let dir = scratch("old-token");
        let path = dir.0.join("ui.sock.token");
        let file = std::fs::symlink_metadata(&dir.0).unwrap();
        assert!(!file_by_type(&file) && !file_by_mode(&file), "a directory");
        std::fs::write(&path, b"old").unwrap();
        let regular = std::fs::symlink_metadata(&path).unwrap();
        assert!(file_by_type(&regular) && file_by_mode(&regular));
        // Another user's: refused, and left as it was.
        let owner = regular.uid();
        assert!(matches!(
            clear_old_token(&path, owner.wrapping_add(1)),
            Err(Error::TokenPathNotOurs { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(mint_ui_token_as(&path, owner.wrapping_add(1)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        // A symlink to a file of ours: refused, and neither it nor its
        // target goes.
        std::fs::remove_file(&path).unwrap();
        let target = dir.0.join("target");
        std::fs::write(&target, b"mine").unwrap();
        symlink(&target, &path).unwrap();
        let link = std::fs::symlink_metadata(&path).unwrap();
        assert!(!file_by_type(&link) && !file_by_mode(&link));
        assert!(matches!(
            clear_old_token(&path, owner),
            Err(Error::TokenPathNotOurs { .. })
        ));
        assert!(std::fs::symlink_metadata(&path).is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"mine");
        // Nothing there: nothing to clear.
        std::fs::remove_file(&path).unwrap();
        assert!(clear_old_token(&path, owner).is_ok());
    }

    #[test]
    fn a_ui_token_is_fresh_its_owners_alone_and_never_written_through_a_plant() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("ui-token");
        let socket: SocketPath = dir.0.join("ui.sock").to_string_lossy().parse().unwrap();
        let path = ui_token_path(&socket);
        assert_eq!(path, dir.0.join("ui.sock.token"));
        let first = mint_ui_token(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first.expose());
        assert!(
            first.expose().len() >= 32,
            "{} hex digits",
            first.expose().len()
        );
        let second = mint_ui_token(&path).unwrap();
        assert_ne!(first, second, "a new token each start");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), second.expose());
        // Something that can't simply be removed in its place: refused,
        // and nothing is written into it.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("inside"), b"").unwrap();
        // Refused before any removal, not left to the exclusive create.
        assert!(matches!(
            mint_ui_token(&path),
            Err(Error::TokenPathNotOurs { path: refused }) if refused == path
        ));
        assert_eq!(std::fs::read(path.join("inside")).unwrap(), b"");
    }

    #[test]
    fn the_api_is_off_unless_a_socket_is_set_and_holds_no_grant_by_default() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        assert_eq!(parse(&config, &[]).unwrap(), ApiConfig::default());
        assert_eq!(
            parse(&config, &[("HATCHLING_API_SOCKET", "  ")]).unwrap(),
            ApiConfig::default()
        );
        let on = parse(
            &config,
            &[("HATCHLING_API_SOCKET", "/run/user/1000/hatchling-api.sock")],
        )
        .unwrap();
        assert_eq!(
            on.socket.unwrap().as_path(),
            Path::new("/run/user/1000/hatchling-api.sock")
        );
        assert!(on.grants.is_empty());
    }

    #[test]
    fn a_socket_path_is_absolute_and_out_of_the_cells_sight_alone() {
        // The path rules alone: no scheduler, which only the API's
        // sockets need.
        let config = owner_config(None);
        let key = "HATCHLING_OPERATOR_SOCKET";
        for unset in [None, Some(""), Some("  ")] {
            assert_eq!(parse_socket_path(unset, &config, key).unwrap(), None);
        }
        assert_eq!(
            parse_socket_path(Some(" /run/hatchling/x.sock "), &config, key).unwrap(),
            Some("/run/hatchling/x.sock".parse().unwrap())
        );
        for refused in ["x.sock", "/tmp/hatchling-test-api-dummy/x.sock"] {
            assert!(
                matches!(
                    parse_socket_path(Some(refused), &config, key),
                    Err(Error::InvalidConfig {
                        key: "HATCHLING_OPERATOR_SOCKET",
                        ..
                    })
                ),
                "{refused}"
            );
        }
        // The same path for the API needs the activity log.
        assert!(matches!(
            parse_socket(
                Some("/run/hatchling/x.sock"),
                &config,
                "HATCHLING_API_SOCKET"
            ),
            Err(Error::InvalidConfig {
                key: "HATCHLING_API_SOCKET",
                ..
            })
        ));
    }

    #[test]
    fn the_socket_must_be_absolute_out_of_the_cells_sight_and_audited() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        assert!(
            reason(
                parse(&config, &[("HATCHLING_API_SOCKET", "api.sock")]),
                "HATCHLING_API_SOCKET"
            )
            .contains("absolute")
        );
        let inside = format!(
            "{}/api.sock",
            config.socket.as_path().parent().unwrap().display()
        );
        assert!(
            reason(
                parse(&config, &[("HATCHLING_API_SOCKET", &inside)]),
                "HATCHLING_API_SOCKET"
            )
            .contains("which the cell mounts")
        );
        let unaudited = owner_config(None);
        assert!(
            reason(
                parse(&unaudited, &[("HATCHLING_API_SOCKET", "/run/api.sock")]),
                "HATCHLING_API_SOCKET"
            )
            .contains("activity log")
        );
    }

    #[test]
    fn grants_are_labels_the_owner_holds() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        let granted = parse(
            &config,
            &[(
                "HATCHLING_API_GRANTS",
                " goals:query-goals , channel-send:local,",
            )],
        )
        .unwrap();
        assert_eq!(
            granted.grants,
            BTreeSet::from([
                goals("query-goals"),
                ScheduleGrant::ChannelSend {
                    channel: String::from("local")
                }
            ])
        );
        assert!(
            reason(
                parse(&config, &[("HATCHLING_API_GRANTS", "goals:goal-create")]),
                "HATCHLING_API_GRANTS"
            )
            .contains("not a grant the owner holds")
        );
        assert!(
            reason(
                parse(&config, &[("HATCHLING_API_GRANTS", "nonsense")]),
                "HATCHLING_API_GRANTS"
            )
            .contains("names no grant")
        );
    }

    #[test]
    fn a_network_listener_is_never_the_default_and_never_starts_in_this_build() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        let asked = reason(
            parse(&config, &[("HATCHLING_API_NETWORK", "0.0.0.0:8443")]),
            "HATCHLING_API_NETWORK",
        );
        assert!(
            asked.contains("never the default")
                && asked.contains("HATCHLING_API_ALLOW_NETWORK=true"),
            "{asked}"
        );
        for allow in ["true", "1"] {
            let allowed = reason(
                parse(
                    &config,
                    &[
                        ("HATCHLING_API_NETWORK", "127.0.0.1:8443"),
                        ("HATCHLING_API_ALLOW_NETWORK", allow),
                    ],
                ),
                "HATCHLING_API_NETWORK",
            );
            assert!(
                allowed.contains("needs a token provider")
                    && allowed.contains("nothing will listen"),
                "{allowed}"
            );
        }
        assert!(
            parse(
                &config,
                &[
                    ("HATCHLING_API_NETWORK", " "),
                    ("HATCHLING_API_ALLOW_NETWORK", "1")
                ]
            )
            .is_ok(),
            "an empty address asks for nothing"
        );
        assert!(parse(&config, &[("HATCHLING_API_ALLOW_NETWORK", "0")]).is_ok());
        assert!(
            reason(
                parse(&config, &[("HATCHLING_API_ALLOW_NETWORK", "yes")]),
                "HATCHLING_API_ALLOW_NETWORK"
            )
            .contains("not \"yes\"")
        );
    }

    #[test]
    fn the_peer_provider_admits_only_the_owner_with_its_grants_and_ignores_tokens() {
        let (client, server) = UnixStream::pair().unwrap();
        let grants = BTreeSet::from([goals("query-goals")]);
        let owner = PeerProvider::new(real_uid(), grants.clone());
        let token = ApiToken::new(String::from("anything"));
        for presented in [None, Some(&token)] {
            let caller = owner.identify(&server, presented).unwrap();
            assert_eq!(caller.label(), format!("uid {}", real_uid()));
            assert_eq!(caller.grants(), &grants);
        }
        let stranger = PeerProvider::new(
            PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1)),
            grants,
        );
        assert!(stranger.identify(&server, None).is_none());
        drop(client);
    }

    #[test]
    fn only_the_sockets_owner_is_read_from() {
        let (_client, server) = UnixStream::pair().unwrap();
        assert!(from_owner(&server, real_uid()));
        assert!(!from_owner(
            &server,
            PeerUid::from_kernel(real_uid().as_u32().wrapping_add(1))
        ));
    }

    #[test]
    fn admitting_narrows_a_providers_claim_to_the_owners_grants() {
        struct Claims(BTreeSet<ScheduleGrant>);
        impl Provider for Claims {
            fn identify(&self, _stream: &UnixStream, _token: Option<&ApiToken>) -> Option<Caller> {
                Some(Caller::new(String::from("claims"), self.0.clone()))
            }
        }
        struct Nobody;
        impl Provider for Nobody {
            fn identify(&self, _stream: &UnixStream, _token: Option<&ApiToken>) -> Option<Caller> {
                None
            }
        }
        let (_client, server) = UnixStream::pair().unwrap();
        let held = BTreeSet::from([goals("query-goals"), goals("goal-create")]);
        let wide = Claims(BTreeSet::from([
            goals("query-goals"),
            ScheduleGrant::Egress {
                destination: String::from("example.com:443"),
            },
        ]));
        let caller = admit(&wide, &held, &server, None).unwrap();
        assert_eq!(
            caller.grants(),
            &BTreeSet::from([goals("query-goals")]),
            "the egress grant the owner lacks is dropped"
        );
        assert_eq!(
            caller.grant_labels(),
            vec![String::from("goals:query-goals")]
        );
        assert!(caller.holds(&BTreeSet::from([goals("query-goals")])));
        assert!(
            !caller.holds(&held),
            "a caller holding less than the owner does not hold the owner's set"
        );
        assert!(caller.holds(&BTreeSet::new()));
        assert!(admit(&Nobody, &held, &server, None).is_none());
    }

    #[test]
    fn each_setting_reader_does_its_one_job() {
        let config = owner_config(Some("/srv/hatchling/schedules"));
        // The process environment sets none of the API's keys in tests.
        assert_eq!(ApiConfig::from_env(&config).unwrap(), ApiConfig::default());
        assert!(refuse_network(None, None).is_ok());
        assert!(refuse_network(Some("127.0.0.1:1"), None).is_err());
        assert!(refuse_network(Some("127.0.0.1:1"), Some("true")).is_err());
        assert_eq!(
            parse_socket(None, &config, "HATCHLING_API_SOCKET").unwrap(),
            None
        );
        assert_eq!(
            parse_socket(Some("/run/api.sock"), &config, "HATCHLING_API_SOCKET").unwrap(),
            Some("/run/api.sock".parse().unwrap())
        );
        assert!(
            parse_socket(
                Some("/run/api.sock"),
                &owner_config(None),
                "HATCHLING_API_SOCKET"
            )
            .is_err()
        );
    }
}
