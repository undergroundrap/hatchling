#![allow(clippy::redundant_pub_crate)]
//! Shared fixtures for the `hatchling-agent` integration suites.
//!
//! The fixture broker speaks the real handshake protocol over a real Unix
//! socket, so component and end-to-end tests exercise the same bytes a
//! production broker would send. It is deliberately minimal: it answers
//! exactly one connection and knows nothing about the agent's internals.
//!
//! Note: `pub(crate)` below is the honest visibility (crate-visible,
//! shared by the integration targets), but `redundant_pub_crate`
//! misjudges the parent module in test targets and contradicts
//! `unreachable_pub`, so the noisier lint is allowed here.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use hatchling_agent::{AgentMessage, BrokerMessage, LogLevel, PROTOCOL_VERSION, PeerUid};
use hatchling_broker::{
    Config as BrokerConfig, EgressConfig, MemoryConfig, SkillConfig, VaultConfig,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{ChannelResult, SessionRequest, SessionResponse, VaultResult};

/// A test's scratch path, file or directory, removed on drop so a
/// failing assertion cannot leak it.
#[allow(
    dead_code,
    reason = "only some integration targets create scratch paths"
)]
pub(crate) struct Scratch(PathBuf);

#[allow(
    dead_code,
    reason = "only some integration targets create scratch paths"
)]
impl Scratch {
    /// Guards `path`, clearing anything a previous run left there.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        let scratch = Self(path.into());
        scratch.remove();
        scratch
    }

    fn remove(&self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.remove();
    }
}

/// How the fixture broker answers the agent's `HELLO`.
///
/// Each integration target uses a different subset of behaviors, so
/// some variants are dead in any single target's build.
#[allow(
    dead_code,
    reason = "each integration target uses a different subset of fixture behaviors"
)]
pub(crate) enum BrokerBehavior {
    /// Verify the `HELLO` and answer `WELCOME`.
    Welcome,
    /// Answer `REJECT` carrying the given reason.
    Reject(String),
    /// Run the real broker pipeline ([`serve_connection`](hatchling_broker::serve_connection))
    /// with these vault grants. The full trust boundary applies: kernel
    /// peer gate, grant mediation, session loop.
    Vault(Vec<hatchling_broker::VaultGrant>),
    /// Run the real broker pipeline with egress allowlist and grants.
    /// The broker mints surrogates for allowlisted destinations.
    Egress {
        /// Comma-separated `host:port` allowlist.
        allowlist: String,
        /// Explicit egress grants.
        grants: Vec<String>,
    },
    /// [`Egress`](Self::Egress), with these vault grants as well
    ///: a model run that both fetches secrets and reaches its
    /// endpoint.
    EgressWithVault {
        /// Comma-separated `host:port` allowlist.
        allowlist: String,
        /// Explicit egress grants.
        grants: Vec<String>,
        /// Explicit vault grants.
        vault: Vec<hatchling_broker::VaultGrant>,
    },
    /// Complete the handshake, then answer every vault op with this
    /// canned result. Pins the agent's reply mapping, including results
    /// that never match the op.
    Canned(VaultResult),
    /// Complete the handshake, then answer the next vault op with these
    /// raw bytes. Pins the agent's malformed-reply path.
    Raw(Vec<u8>),
    /// Complete the handshake, then close without answering. Pins the
    /// agent's mid-request EOF path.
    Hangup,
    /// Run the real broker pipeline with a memory store at `root` and
    /// these grants; vault and egress are empty.
    Memory {
        /// The memory store root (absolute; the broker creates it 0700).
        root: PathBuf,
        /// Explicit (scope, action) memory grants.
        grants: Vec<hatchling_broker::MemoryGrant>,
    },
    /// Run the real broker pipeline with a goals store and a memory
    /// store (the query path's memory reads); vault and egress are
    /// empty.
    Goals {
        /// The goals store root and grants.
        goals: hatchling_broker::GoalsConfig,
        /// The memory store root and grants.
        memory: MemoryConfig,
    },
    /// Complete the handshake, then answer each session request with
    /// the next scripted channel result, recording every request in
    /// `seen`. When the script runs out, record the next request and
    /// hang up. This pins the agent's channel reply mapping and the ops
    /// it sends; the real broker's turn, through its ingress, is in
    /// `channel_ingress.rs`.
    CannedChannel {
        /// Replies, in order, each wrapped in `SessionResponse::Channel`.
        replies: Vec<ChannelResult>,
        /// Every session request the fixture read, in order.
        seen: Arc<Mutex<Vec<SessionRequest>>>,
    },
}

/// A fixture broker: a real Unix socket served by a background thread.
///
/// The thread serves exactly one connection, then exits. Dropping the
/// broker unblocks the listener (via a dummy connection, which the serve
/// loop treats as teardown) and joins the thread, so a test that never
/// connects cannot hang the suite.
pub(crate) struct FixtureBroker {
    /// Filesystem path of the listening socket.
    pub(crate) socket_path: String,
    handle: Option<JoinHandle<()>>,
    vault_root: Option<PathBuf>,
}

impl FixtureBroker {
    /// Bind a fresh socket for `name` and start serving in the background.
    ///
    /// `name` must be unique within the test binary; the process id is
    /// mixed into the path so parallel `cargo test` runs do not collide.
    ///
    /// The socket lives directly under `/tmp` rather than
    /// `std::env::temp_dir()`: `sun_path` is capped at 107 bytes and
    /// `TMPDIR` is environment-controlled (the mutation runner points it at
    /// a deep scratch directory, which made fixture binds fail with
    /// `SUN_LEN` errors). Only the socket lives here; bulkier fixture state
    /// such as vault roots stays on the big filesystem via `temp_dir()`.
    pub(crate) fn spawn(name: &str, behavior: BrokerBehavior) -> std::io::Result<Self> {
        let path = PathBuf::from(format!("/tmp/hlfx-{}-{name}.sock", std::process::id()));
        let socket_path = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        // The real-broker behaviors need a vault root; derive it from the
        // test name so both this thread and the broker thread agree, and
        // so `Drop` removes exactly this test's root.
        let real_broker = matches!(
            behavior,
            BrokerBehavior::Vault(_)
                | BrokerBehavior::Goals { .. }
                | BrokerBehavior::Memory { .. }
                | BrokerBehavior::Egress { .. }
                | BrokerBehavior::EgressWithVault { .. }
        );
        let vault_root = real_broker.then(|| {
            let mut root = std::env::temp_dir();
            root.push(format!(
                "hatchling-fixture-vault-{}-{name}",
                std::process::id()
            ));
            root
        });
        if let Some(root) = &vault_root {
            let _ = std::fs::remove_dir_all(root);
        }
        let thread_root = vault_root.clone();
        let handle = thread::spawn(move || serve(listener, behavior, thread_root));
        Ok(Self {
            socket_path,
            handle: Some(handle),
            vault_root,
        })
    }

    /// The vault root the real broker used, if this fixture ran one.
    /// Exposed so tests can assert on the store's on-disk state. Only
    /// the vault suite uses it.
    #[allow(dead_code, reason = "only the vault integration target uses this")]
    pub(crate) const fn vault_root(&self) -> Option<&PathBuf> {
        self.vault_root.as_ref()
    }
}

impl Drop for FixtureBroker {
    fn drop(&mut self) {
        // Unblock `accept` when the test never connected: the dummy
        // connection fails the HELLO read, which `serve` treats as
        // teardown and exits.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(root) = &self.vault_root {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// Serve one connection, then exit.
///
/// Any anomaly — unreadable HELLO, wrong version, serialization failure —
/// ends the session quietly. The agent under test observes EOF or a
/// missing reply and fails closed; the test's assertions on the agent's
/// outcome are the real checks.
#[allow(
    clippy::too_many_lines,
    reason = "fixture dispatch: each behavior needs its setup inline"
)]
fn serve(listener: UnixListener, behavior: BrokerBehavior, vault_root: Option<PathBuf>) {
    let Ok((server, _)) = listener.accept() else {
        return;
    };
    // One connection is the whole contract; the listener is done.
    drop(listener);
    // Egress alone is egress with no vault grants.
    let behavior = if let BrokerBehavior::Egress { allowlist, grants } = behavior {
        BrokerBehavior::EgressWithVault {
            allowlist,
            grants,
            vault: Vec::new(),
        }
    } else {
        behavior
    };

    // The Vault and Egress behaviors delegate the entire connection —
    // handshake, peer gate, session — to the real broker pipeline.
    if let BrokerBehavior::Vault(grants) = behavior {
        let Some(root) = vault_root else {
            // `spawn` always sets a root for the Vault behavior; a
            // missing one is a fixture bug, not a serving failure.
            return;
        };
        let socket: hatchling_core::SocketPath = match "/tmp/hatchling-fixture-dummy.sock".parse() {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let key_file = VaultConfig::default_key_file(&root);
        let config = BrokerConfig {
            socket,
            agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
            socket_mode: 0o600,
            max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
            scheduler: hatchling_broker::SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                root,
                key_file,
                grants,
            },
            egress: EgressConfig {
                allowlist: hatchling_broker::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: hatchling_broker::ArtifactConfig::disabled(),
            spaces: hatchling_broker::SpaceConfig::disabled(),
            web: hatchling_broker::WebConfig::off(),
            goals: hatchling_broker::GoalsConfig::disabled(),
        };
        // The broker's single master key, borrowed by the sealed vault
        // for the session.
        let Ok(key) = hatchling_vault::MasterKey::generate() else {
            return;
        };
        let _ = hatchling_broker::serve_connection(
            &server,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
        );
        return;
    }

    // The Goals behavior runs the real broker with a goals store and a
    // memory store configured. Vault and egress are empty.
    if let BrokerBehavior::Goals { goals, memory } = behavior {
        let socket: hatchling_core::SocketPath = match "/tmp/hatchling-fixture-dummy.sock".parse() {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let Some(vault_root) = vault_root else {
            return;
        };
        let _ = std::fs::create_dir_all(&vault_root);
        let key_file = VaultConfig::default_key_file(&vault_root);
        let config = BrokerConfig {
            socket,
            agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
            socket_mode: 0o600,
            max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
            scheduler: hatchling_broker::SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                root: vault_root,
                key_file,
                grants: Vec::new(),
            },
            egress: EgressConfig {
                allowlist: hatchling_broker::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory,
            artifacts: hatchling_broker::ArtifactConfig::disabled(),
            spaces: hatchling_broker::SpaceConfig::disabled(),
            web: hatchling_broker::WebConfig::off(),
            goals,
        };
        let Ok(key) = hatchling_vault::MasterKey::generate() else {
            return;
        };
        let _ = hatchling_broker::serve_connection(
            &server,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
        );
        return;
    }

    // The Memory behavior runs the real broker with a memory store and
    // grants configured. Vault and egress are empty.
    if let BrokerBehavior::Memory { root, grants } = behavior {
        let socket: hatchling_core::SocketPath = match "/tmp/hatchling-fixture-dummy.sock".parse() {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let Some(vault_root) = vault_root else {
            return;
        };
        let _ = std::fs::create_dir_all(&vault_root);
        let key_file = VaultConfig::default_key_file(&vault_root);
        let config = BrokerConfig {
            socket,
            agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
            socket_mode: 0o600,
            max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
            scheduler: hatchling_broker::SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                root: vault_root,
                key_file,
                grants: Vec::new(),
            },
            egress: EgressConfig {
                allowlist: hatchling_broker::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory: MemoryConfig { root, grants },
            artifacts: hatchling_broker::ArtifactConfig::disabled(),
            spaces: hatchling_broker::SpaceConfig::disabled(),
            web: hatchling_broker::WebConfig::off(),
            goals: hatchling_broker::GoalsConfig::disabled(),
        };
        let Ok(key) = hatchling_vault::MasterKey::generate() else {
            return;
        };
        let _ = hatchling_broker::serve_connection(
            &server,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
        );
        return;
    }

    // The Egress behaviors run the real broker with egress allowlist
    // and grants configured, and these vault grants.
    if let BrokerBehavior::EgressWithVault {
        allowlist,
        grants,
        vault,
    } = behavior
    {
        let socket: hatchling_core::SocketPath = match "/tmp/hatchling-fixture-dummy.sock".parse() {
            Ok(socket) => socket,
            Err(_) => return,
        };
        // Vault root is required by the broker config; the fixture's
        // own root, discarded on drop (egress tests don't use the vault).
        let Some(root) = vault_root else {
            return;
        };
        let _ = std::fs::create_dir_all(&root);
        // The vault refuses a root more open than its own 0700.
        let _ =
            std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let key_file = VaultConfig::default_key_file(&root);
        let config = BrokerConfig {
            socket,
            agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
            socket_mode: 0o600,
            max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
            scheduler: hatchling_broker::SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                root,
                key_file,
                grants: vault,
            },
            egress: EgressConfig {
                allowlist: hatchling_broker::EgressAllowlist::new(allowlist),
                grants,
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: hatchling_broker::ArtifactConfig::disabled(),
            spaces: hatchling_broker::SpaceConfig::disabled(),
            web: hatchling_broker::WebConfig::off(),
            goals: hatchling_broker::GoalsConfig::disabled(),
        };
        let Ok(key) = hatchling_vault::MasterKey::generate() else {
            return;
        };
        let _ = hatchling_broker::serve_connection(
            &server,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
        );
        return;
    }

    let Ok(frame) = read_frame(&server) else {
        // Teardown connection (e.g. the drop guard's dummy connect):
        // nothing to serve.
        return;
    };
    let hello: AgentMessage = match serde_json::from_slice(&frame) {
        Ok(message) => message,
        Err(_) => return,
    };
    let reply = match &behavior {
        BrokerBehavior::Reject(reason) => BrokerMessage::Reject {
            v: PROTOCOL_VERSION,
            reason: reason.clone(),
        },
        BrokerBehavior::Vault(_)
        | BrokerBehavior::Egress { .. }
        | BrokerBehavior::EgressWithVault { .. }
        | BrokerBehavior::Memory { .. }
        | BrokerBehavior::Goals { .. } => {
            unreachable!("handled above")
        }
        // Welcome, Canned, Raw, and Hangup all complete the handshake
        // the same way; they differ only in what happens *after*.
        BrokerBehavior::Welcome
        | BrokerBehavior::Canned(_)
        | BrokerBehavior::Raw(_)
        | BrokerBehavior::Hangup
        | BrokerBehavior::CannedChannel { .. } => {
            if !matches!(
                hello,
                AgentMessage::Hello {
                    v: PROTOCOL_VERSION,
                    ..
                }
            ) {
                return;
            }
            BrokerMessage::Welcome {
                v: PROTOCOL_VERSION,
            }
        }
    };
    if let Ok(bytes) = serde_json::to_vec(&reply) {
        let _ = write_frame(&server, &bytes);
    }

    // Post-handshake behaviors for pinning the agent's vault reply
    // mapping. The agent sends exactly one op frame next. Every
    // behavior is enumerated: a new behavior must be a conscious
    // decision here, not a silent fallthrough.
    match behavior {
        BrokerBehavior::Canned(result) => {
            if read_frame(&server).is_ok() {
                // The v3 session envelope: canned vault results are
                // wrapped in `SessionResponse::Vault`.
                let response = SessionResponse::Vault(result);
                if let Ok(bytes) = serde_json::to_vec(&response) {
                    let _ = write_frame(&server, &bytes);
                }
            }
        }
        BrokerBehavior::Raw(bytes) => {
            if read_frame(&server).is_ok() {
                let _ = write_frame(&server, &bytes);
            }
        }
        BrokerBehavior::CannedChannel { replies, seen } => {
            let record = |frame: &[u8]| {
                if let Ok(request) = serde_json::from_slice::<SessionRequest>(frame) {
                    if let Ok(mut guard) = seen.lock() {
                        guard.push(request);
                    }
                }
            };
            for reply in replies {
                let Ok(frame) = read_frame(&server) else {
                    return;
                };
                record(&frame);
                let Ok(bytes) = serde_json::to_vec(&SessionResponse::Channel(reply)) else {
                    return;
                };
                if write_frame(&server, &bytes).is_err() {
                    return;
                }
            }
            // Script exhausted: record the next request, then hang up.
            if let Ok(frame) = read_frame(&server) {
                record(&frame);
            }
        }
        // Hangup: drop the connection without answering; the agent must
        // see EOF and fail closed.
        BrokerBehavior::Hangup
        | BrokerBehavior::Welcome
        | BrokerBehavior::Reject(_)
        | BrokerBehavior::Vault(_)
        | BrokerBehavior::Egress { .. }
        | BrokerBehavior::EgressWithVault { .. }
        | BrokerBehavior::Memory { .. }
        | BrokerBehavior::Goals { .. } => {}
    }
}
