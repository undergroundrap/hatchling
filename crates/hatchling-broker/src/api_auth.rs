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
//! - `HATCHLING_API_GRANTS`: the local caller's grants, as grant labels
//!   (`goals:query-goals,channel-send:local`). Each must be one the owner holds.
//!   Unset means none: the caller can follow the activity, and do only
//!   what needs no grant.
//! - `HATCHLING_API_NETWORK`: a network listener's address. Never the
//!   default, and refused unless `HATCHLING_API_ALLOW_NETWORK` is `true`;
//!   even then refused in this build, which has no token provider to
//!   identify a network caller.

use std::collections::BTreeSet;
use std::os::unix::net::UnixStream;

use hatchling_core::messages::ScheduleGrant;
use hatchling_core::subagent::{grant_label, parse_grant_label};
use hatchling_core::ui::ApiToken;
use hatchling_core::{PeerUid, SocketPath, peer_credentials};

use crate::schedule_ops::held_grants;
use crate::{Config, Error};

/// The API's own settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiConfig {
    /// The API socket, when the API is on.
    pub socket: Option<SocketPath>,
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
            get("HATCHLING_API_NETWORK").as_deref(),
            get("HATCHLING_API_ALLOW_NETWORK").as_deref(),
        )?;
        let grants = parse_grants(get("HATCHLING_API_GRANTS").as_deref(), &held_grants(config))?;
        let socket = parse_socket(get("HATCHLING_API_SOCKET").as_deref(), config)?;
        Ok(Self { socket, grants })
    }
}

/// A strict boolean setting: `true`/`1` or `false`/`0`, unset meaning
/// false.
fn flag(raw: Option<&str>, key: &'static str) -> Result<bool, Error> {
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

/// The API socket: absolute, outside the cell's sight, with an activity
/// log to audit to.
fn parse_socket(raw: Option<&str>, config: &Config) -> Result<Option<SocketPath>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_API_SOCKET",
        reason,
    };
    let socket: SocketPath = raw
        .parse()
        .map_err(|_| invalid(format!("the API socket must be an absolute path: {raw:?}")))?;
    if let Some(cell_visible) = config.socket.as_path().parent() {
        if socket.as_path().starts_with(cell_visible) {
            return Err(invalid(format!(
                "the API socket {raw:?} is inside the broker socket directory {}, which the cell mounts",
                cell_visible.display()
            )));
        }
    }
    if !config.scheduler.enabled() {
        return Err(invalid(String::from(
            "every API call is recorded in the activity log, which needs HATCHLING_SCHEDULER_DIR",
        )));
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
    pub(crate) struct Scratch(pub(crate) std::path::PathBuf);

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
        let root = std::path::PathBuf::from("/nonexistent/hatchling-api-vault");
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
                root: std::path::PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: crate::ArtifactConfig::disabled(),
            spaces: crate::SpaceConfig::disabled(),
            goals: crate::GoalsConfig::disabled(),
        };
        config.goals.grants = vec![crate::GoalsAction::QueryGoals];
        if let Some(root) = scheduler {
            config.scheduler.root = std::path::PathBuf::from(root);
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
            std::path::Path::new("/run/user/1000/hatchling-api.sock")
        );
        assert!(on.grants.is_empty());
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
        assert_eq!(parse_socket(None, &config).unwrap(), None);
        assert_eq!(
            parse_socket(Some("/run/api.sock"), &config).unwrap(),
            Some("/run/api.sock".parse().unwrap())
        );
        assert!(parse_socket(Some("/run/api.sock"), &owner_config(None)).is_err());
    }
}
