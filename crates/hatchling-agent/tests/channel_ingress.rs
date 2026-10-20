//! End to end: a message injected at the broker's ingress
//! socket reaches the agent through a real broker session, the agent's
//! channel turn answers it, and the reply comes back out of the ingress.
//!
//! Everything on the path is real: the ingress socket and its peer gate,
//! the broker's session pipeline and channel registry, the agent's cell
//! handshake, channel client, and loop. Only the model is a test double,
//! so the turn is deterministic.
//!
//! No step waits on a timer. The ingress answers every injected message
//! with `queued` once it is in the hub, so the turn starts only after
//! the message can be received. A reply is written to the ingress client
//! before the broker answers the agent's send, so it is already
//! readable when the turn returns. The read timeouts only bound a
//! failure; they never pace a success.
#![allow(
    clippy::unwrap_used,
    reason = "fixture setup: a failed bind, spawn, or handshake must fail the test loudly"
)]

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use hatchling_agent::{Cell, ChannelClient, Config, LogLevel, PeerUid, Ready};
use hatchling_broker::{
    Config as BrokerConfig, EgressConfig, GoalsConfig, Ingress, MemoryConfig, SkillConfig,
    VaultConfig, serve_connection_with_ingress,
};
use hatchling_channel::{ChannelId, IngressEvent, Message, OutputGate, SenderId};
use hatchling_core::SocketPath;
use hatchling_core::framing::{read_frame, write_frame};

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Socket paths live under `/tmp`: `sun_path` is short and `TMPDIR` may
/// be deep. The process id keeps parallel runs apart, and the guard
/// removes the socket when the test ends, pass or fail.
fn socket(name: &str) -> SocketFile {
    let path = PathBuf::from(format!("/tmp/hlie-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    SocketFile(path)
}

/// A test socket under `/tmp`, removed on drop.
struct SocketFile(PathBuf);

impl std::ops::Deref for SocketFile {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for SocketFile {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn broker_config(name: &str) -> BrokerConfig {
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-ingress-e2e-{}-{name}",
        std::process::id()
    ));
    BrokerConfig {
        socket: "/tmp/hatchling-test-ingress-e2e-dummy.sock"
            .parse()
            .unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&vault_root),
            root: vault_root,
            grants: Vec::new(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        // The hub is handed to the session directly below; this field is
        // what `run_serve` would bind it from.
        channel_ingress: None,
        skills: SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: GoalsConfig::disabled(),
    }
}

/// A `Ready` agent cell against the broker socket at `path`.
fn agent(path: &str) -> Cell<Ready> {
    let config = Config {
        cell_id: "cell-ingress".parse().unwrap(),
        broker: hatchling_agent::BrokerAddress::Socket(path.parse().unwrap()),
        broker_uid: PeerUid::from_kernel(real_uid()),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
    };
    Cell::new(config)
        .check_preconditions()
        .unwrap()
        .connect_broker()
        .unwrap()
        .authorize()
        .unwrap()
        .handshake()
        .unwrap()
}

/// Injects `msg` at the ingress and returns the ingress's answer.
fn inject(ingress: &UnixStream, msg: &Message) -> IngressEvent {
    write_frame(ingress, &serde_json::to_vec(msg).unwrap()).unwrap();
    next_event(ingress)
}

fn next_event(ingress: &UnixStream) -> IngressEvent {
    serde_json::from_slice(&read_frame(ingress).unwrap()).unwrap()
}

/// A model that records what it is shown and answers once.
struct Answering {
    answer: &'static str,
    shown: Mutex<Vec<Vec<hatchling_loop::Message>>>,
}

impl hatchling_loop::Model for Answering {
    fn next_action(
        &self,
        messages: &[hatchling_loop::Message],
        _tools: &[hatchling_loop::ToolDescription],
    ) -> Result<hatchling_loop::ModelAction, hatchling_loop::Error> {
        if let Ok(mut shown) = self.shown.lock() {
            shown.push(messages.to_vec());
        }
        Ok(hatchling_loop::ModelAction::Text(String::from(self.answer)))
    }
}

#[test]
fn an_injected_message_is_answered_through_a_real_session() {
    let ingress_path = socket("ingress");
    let broker_path = socket("broker");
    let ingress_socket: SocketPath = ingress_path.to_str().unwrap().parse().unwrap();
    let ingress = Ingress::bind(&ingress_socket, OutputGate::disabled()).unwrap();
    let hub = ingress.hub().clone();
    let listener = UnixListener::bind(&broker_path).unwrap();
    let config = broker_config("turn");
    let key = hatchling_vault::MasterKey::generate().unwrap();

    std::thread::scope(|scope| {
        let ingress_served = scope.spawn(|| ingress.serve_one());
        let session = scope.spawn(|| {
            let (stream, _) = listener.accept().unwrap();
            serve_connection_with_ingress(
                &stream,
                &config,
                &key,
                &hatchling_broker::Skills::default(),
                Some(&hub),
            )
        });

        // The local user connects to the ingress.
        let user = UnixStream::connect(&ingress_path).unwrap();
        user.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let cell = agent(broker_path.to_str().unwrap());
        let client = ChannelClient::new(cell.broker(), ChannelId::local());

        // 1. The turn: inject, and the agent's real channel turn answers.
        //    The ingress claims a sender and a channel; neither survives.
        let question = Message {
            sender: SenderId(String::from("mallory")),
            channel: ChannelId(String::from("telegram")),
            text: String::from("what is 2 + 2?"),
        };
        assert_eq!(inject(&user, &question), IngressEvent::Queued);
        let model = Answering {
            answer: "4",
            shown: Mutex::new(Vec::new()),
        };
        let turn = cell
            .run_channel_turn_with_model(&client, &model, 3)
            .unwrap()
            .expect("a queued message runs a turn");
        assert_eq!(turn.answer, "4");
        let shown = model.shown.lock().unwrap().clone();
        assert!(
            matches!(
                shown.first().map(Vec::as_slice),
                Some([hatchling_loop::Message::System(_), hatchling_loop::Message::User(task)])
                    if task == "what is 2 + 2?"
            ),
            "the injected text is the user task: {shown:?}"
        );
        // Observed at the ingress: the agent's answer, as a reply.
        assert_eq!(
            next_event(&user),
            IngressEvent::Reply {
                text: String::from("4")
            }
        );

        // 2. The identity the agent received (F-2): the broker's stamp as
        //    sender and the registry key as channel, not the claims.
        assert_eq!(inject(&user, &question), IngressEvent::Queued);
        let received = client.receive().unwrap().expect("the queued message");
        assert_eq!(
            received,
            Message {
                sender: SenderId(format!("uid:{}", real_uid())),
                channel: ChannelId::local(),
                text: String::from("what is 2 + 2?"),
            }
        );
        client.send_reply(String::from("still 4")).unwrap();
        assert_eq!(
            next_event(&user),
            IngressEvent::Reply {
                text: String::from("still 4")
            }
        );

        // 3. Nothing is left: an empty channel still answers no message,
        //    and a reply no message earned is still refused.
        assert_eq!(client.receive().unwrap(), None);
        assert!(client.send_reply(String::from("unasked")).is_err());

        drop(user);
        ingress_served.join().unwrap().unwrap();
        drop(client);
        drop(cell);
        session.join().unwrap().unwrap();
    });
}

/// A held secret the gated turn's model leaks, reworded.
const LEAKED: &str = "nightjar-7f3a91c2";

///, end to end: the agent's real channel turn answers with a
/// secret the broker holds, spaced out so no exact match finds it. The
/// user reads only the gate's notice, the agent learns the category and
/// the event id and nothing else, and the audit log records the event by
/// hashes, never its text. A clean reply after it still goes through.
#[test]
fn a_reply_carrying_a_held_secret_reaches_the_user_as_the_notice_only() {
    let ingress_path = socket("gated-ingress");
    let broker_path = socket("gated-broker");
    let mut config = broker_config("gated");
    config.vault.grants = vec![hatchling_broker::VaultGrant {
        action: hatchling_core::VaultAction::Fetch,
        name: "app-token".parse().unwrap(),
    }];
    let deployment = Deployment(config.vault.root.with_extension("gated-redactions"));
    let _ = std::fs::remove_dir_all(&config.vault.root);
    let _vault = Deployment(config.vault.root.clone());
    let key = hatchling_vault::MasterKey::generate().unwrap();
    hatchling_vault::SealedVault::open(&config.vault.root, &key)
        .unwrap()
        .store(
            &"app-token".parse().unwrap(),
            &hatchling_vault::Secret::new(LEAKED.as_bytes().to_vec()).unwrap(),
        )
        .unwrap();
    hatchling_broker::redactions::ensure_dir(&deployment.0).unwrap();
    let (handle, worker) = hatchling_broker::output_gate::rules_channel(Duration::from_secs(5));
    let rules = hatchling_broker::Rules::new(
        &config,
        &key,
        hatchling_broker::output_gate::Builtins::ALL,
        Vec::new(),
    );
    let raw = deployment.0.join("raw");

    // The rules worker outlives the gate: it stops once the hub, and the
    // handle in its gate, are dropped at the end of the inner block.
    std::thread::scope(|outer| {
        outer.spawn(move || worker.serve(&rules, &raw));
        let gate = OutputGate::new(
            std::sync::Arc::new(handle),
            std::sync::Arc::new(hatchling_broker::FileAudit::new(&deployment.0, None)),
            std::sync::Arc::new(hatchling_broker::FileOverrides::new(&deployment.0)),
            Duration::from_secs(5),
        );
        let ingress_socket: SocketPath = ingress_path.to_str().unwrap().parse().unwrap();
        let ingress = Ingress::bind(&ingress_socket, gate).unwrap();
        let hub = ingress.hub().clone();
        let listener = UnixListener::bind(&broker_path).unwrap();
        std::thread::scope(|scope| {
            gated_turn(
                scope,
                &ingress,
                &hub,
                &listener,
                &config,
                &key,
                (&ingress_path, &broker_path),
                &deployment.0,
            );
        });
    });
}

/// The gated turn of [`a_reply_carrying_a_held_secret_reaches_the_user_as_the_notice_only`].
#[allow(
    clippy::too_many_arguments,
    reason = "the test's fixtures, borrowed in one place"
)]
fn gated_turn<'s>(
    scope: &'s std::thread::Scope<'s, '_>,
    ingress: &'s Ingress,
    hub: &'s hatchling_channel::LocalHub,
    listener: &'s UnixListener,
    config: &'s BrokerConfig,
    key: &'s hatchling_vault::MasterKey,
    paths: (&std::path::Path, &std::path::Path),
    dir: &std::path::Path,
) {
    let (ingress_path, broker_path) = paths;
    let ingress_served = scope.spawn(|| ingress.serve_one());
    let session = scope.spawn(|| {
        let (stream, _) = listener.accept().unwrap();
        serve_connection_with_ingress(
            &stream,
            config,
            key,
            &hatchling_broker::Skills::default(),
            Some(hub),
        )
    });
    let user = UnixStream::connect(ingress_path).unwrap();
    user.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let cell = agent(broker_path.to_str().unwrap());
    let client = ChannelClient::new(cell.broker(), ChannelId::local());

    let question = Message {
        sender: SenderId(String::from("someone")),
        channel: ChannelId::local(),
        text: String::from("what is the token?"),
    };
    assert_eq!(inject(&user, &question), IngressEvent::Queued);
    let model = Answering {
        answer: "it is NIGHTJAR 7F3A 91C2, keep it safe",
        shown: Mutex::new(Vec::new()),
    };
    let error = cell
        .run_channel_turn_with_model(&client, &model, 3)
        .unwrap_err();
    let hatchling_agent::Error::ChannelRedacted {
        channel,
        category,
        event_id,
    } = &error
    else {
        unreachable!("the turn was not redacted: {error:?}");
    };
    assert_eq!((channel.as_str(), category.as_str()), ("local", "secret"));
    let IngressEvent::Reply { text } = next_event(&user) else {
        unreachable!("no reply reached the user");
    };
    let secret = hatchling_channel::Category::new("secret").unwrap();
    assert_eq!(text, hatchling_channel::notice(&secret, event_id));
    let shown = error.to_string().to_lowercase();
    assert!(!shown.contains("nightjar"), "{shown}");

    // The log: this event, by hashes; no form of the secret.
    let log = std::fs::read_to_string(dir.join("audit.log")).unwrap();
    let record: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(record.get("kind"), Some(&serde_json::json!("redacted")));
    assert_eq!(record.get("event_id"), Some(&serde_json::json!(event_id)));
    assert_eq!(record.get("category"), Some(&serde_json::json!("secret")));
    assert_eq!(record.get("raw"), Some(&serde_json::json!("off")));
    assert!(
        record
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|scope| scope.starts_with("session:")),
        "{record}"
    );
    let lower = log.to_lowercase();
    assert!(
        !lower.contains("nightjar") && !lower.contains("7f3a"),
        "{log}"
    );

    // A clean reply in the same session still goes through.
    assert_eq!(inject(&user, &question), IngressEvent::Queued);
    assert!(client.receive().unwrap().is_some(), "the queued message");
    client
        .send_reply(String::from("I cannot share that."))
        .unwrap();
    assert_eq!(
        next_event(&user),
        IngressEvent::Reply {
            text: String::from("I cannot share that.")
        }
    );

    drop(user);
    ingress_served.join().unwrap().unwrap();
    drop(client);
    drop(cell);
    session.join().unwrap().unwrap();
}

/// A test directory, removed with everything in it on drop.
struct Deployment(PathBuf);

impl Drop for Deployment {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
