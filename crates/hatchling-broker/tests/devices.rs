//! Component: device pairing, sync, and media, through the real
//! device endpoint's handshake and the broker's real space ops on the real
//! sealed store. A test device holds a real Ed25519 key and checks the
//! broker's signature as a device would.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use hatchling_broker::{
    Config, EgressConfig, GoalsConfig, MemoryConfig, SchedulerConfig, SealedSpaceStore,
    SpaceConfig, SpaceSession, VaultConfig, handle_space_op, pairing_refusals, serve_device,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    DeviceChallenge, DeviceHello, DeviceRefusal, DeviceWelcome, LogLevel, PROTOCOL_VERSION,
    PeerUid, SpaceOp, SpaceRefusal, SpaceResult, SyncChange, SyncItem,
};
use hatchling_scheduler::Timestamp;
use hatchling_space::{
    PairingRefusal, PeerPin, Principal, SessionPin, auth_transcript, broker_transcript, device_id,
    pair_transcript, parse_key, verifies,
};
use hatchling_vault::MasterKey;

/// One master key for the process, as the broker has one.
fn key() -> &'static MasterKey {
    static KEY: OnceLock<MasterKey> = OnceLock::new();
    KEY.get_or_init(|| MasterKey::generate().unwrap())
}

/// The wall clock when the process first asks: the endpoint redeems codes
/// on the wall clock, so the handler's ops share that timeline.
fn now_secs() -> i64 {
    static NOW: OnceLock<i64> = OnceLock::new();
    *NOW.get_or_init(|| Timestamp::now().as_second())
}

fn at(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap()
}

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Removes a test's base directory when dropped.
struct Base(PathBuf);

impl Drop for Base {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A broker config with a space store under a fresh base.
fn config(name: &str) -> (Base, Config) {
    let base = std::env::temp_dir().join(format!(
        "hatchling-test-device-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let vault_root = base.join("vault");
    let config = Config {
        socket: "/tmp/hatchling-test-device-dummy.sock".parse().unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: SchedulerConfig::disabled(),
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
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: SpaceConfig {
            root: base.join("spaces"),
            bound: None,
            device_socket: None,
        },
        goals: GoalsConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
    };
    (Base(base), config)
}

/// The agent's session in test `name`.
fn agent(name: &str) -> SpaceSession {
    SpaceSession {
        principal: Principal::Agent { uid: 1000 },
        pin: SessionPin {
            session: format!("{name}-agent"),
            peer: PeerPin { uid: 1000, pid: 10 },
        },
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers build the op inline; taking it owned keeps them terse"
)]
fn op_at(config: &Config, who: &SpaceSession, op: SpaceOp, when: i64) -> SpaceResult {
    handle_space_op(config, key(), who, &op, at(when))
}

fn op(config: &Config, who: &SpaceSession, op: SpaceOp) -> SpaceResult {
    op_at(config, who, op, now_secs())
}

const fn refused(reason: SpaceRefusal) -> SpaceResult {
    SpaceResult::Refused { reason }
}

/// A space the agent created and joined, and its ticket.
fn agent_space(config: &Config, who: &SpaceSession, when: i64) -> (String, String) {
    let created = op_at(
        config,
        who,
        SpaceOp::Create {
            name: String::from("Shared"),
            grants: Vec::new(),
        },
        when,
    );
    let SpaceResult::Created { space } = created else {
        return (format!("not created: {created:?}"), String::new());
    };
    let joined = op_at(
        config,
        who,
        SpaceOp::Join {
            space: space.clone(),
        },
        when,
    );
    let SpaceResult::Admitted { ticket, .. } = joined else {
        return (space, format!("not admitted: {joined:?}"));
    };
    (space, ticket)
}

/// A pairing code for `space`, issued at `when`.
fn pairing_code(
    config: &Config,
    who: &SpaceSession,
    space: &str,
    ticket: &str,
    when: i64,
) -> String {
    let issued = op_at(
        config,
        who,
        SpaceOp::Pair {
            space: space.to_owned(),
            ticket: ticket.to_owned(),
        },
        when,
    );
    if let SpaceResult::PairingCode { code, .. } = issued {
        code
    } else {
        format!("no code: {issued:?}")
    }
}

fn put(space: &str, ticket: &str, key: &str, value: &str) -> SpaceOp {
    SpaceOp::Put {
        space: space.to_owned(),
        ticket: ticket.to_owned(),
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn get(space: &str, ticket: &str, key: &str) -> SpaceOp {
    SpaceOp::Get {
        space: space.to_owned(),
        ticket: ticket.to_owned(),
        key: key.to_owned(),
    }
}

/// The device's own nonce, a fixed shape.
fn device_nonce() -> String {
    "5a".repeat(32)
}

/// A connection to the device endpoint, served on a thread for a peer of
/// the test's own uid; returns the stream and the broker's challenge.
fn connect(config: &Config) -> (UnixStream, DeviceChallenge) {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let config = config.clone();
    std::thread::spawn(move || {
        let _ = serve_device(&server, &config, key(), PeerUid::from_kernel(real_uid()));
    });
    let challenge: DeviceChallenge = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
    assert_eq!(challenge.v, PROTOCOL_VERSION);
    (client, challenge)
}

fn send_hello(stream: &UnixStream, hello: &DeviceHello) -> DeviceWelcome {
    write_frame(stream, &serde_json::to_vec(hello).unwrap()).unwrap();
    serde_json::from_slice(&read_frame(stream).unwrap()).unwrap()
}

fn pair_hello(device: &SigningKey, challenge: &DeviceChallenge, code: &str) -> DeviceHello {
    let device_key = b64(device.verifying_key().as_bytes());
    let signature = device.sign(&pair_transcript(&challenge.nonce, code, &device_key));
    DeviceHello::Pair {
        code: code.to_owned(),
        device_key,
        nonce: device_nonce(),
        signature: b64(&signature.to_bytes()),
    }
}

fn auth_hello(device: &SigningKey, challenge: &DeviceChallenge, id: &str) -> DeviceHello {
    let signature = device.sign(&auth_transcript(&challenge.nonce, id));
    DeviceHello::Auth {
        device: id.to_owned(),
        nonce: device_nonce(),
        signature: b64(&signature.to_bytes()),
    }
}

/// Whether `signature` is the broker's, by the key in its challenge, over
/// the device's nonce and id: what a device checks before trusting it.
fn broker_signed(challenge: &DeviceChallenge, id: &str, signature: &str) -> bool {
    parse_key(&challenge.broker_key)
        .is_some_and(|broker| verifies(&broker, &broker_transcript(&device_nonce(), id), signature))
}

/// One space op on an authenticated device connection.
fn ask(stream: &UnixStream, op: &SpaceOp) -> SpaceResult {
    write_frame(stream, &serde_json::to_vec(op).unwrap()).unwrap();
    serde_json::from_slice(&read_frame(stream).unwrap()).unwrap()
}

/// A device paired into `space` with `code`: its key, id, and live
/// connection.
fn paired(config: &Config, seed: u8, code: &str) -> (SigningKey, String, UnixStream) {
    let device = SigningKey::from_bytes(&[seed; 32]);
    let (stream, challenge) = connect(config);
    let welcome = send_hello(&stream, &pair_hello(&device, &challenge, code));
    let id = device_id(&device.verifying_key());
    assert!(
        matches!(&welcome, DeviceWelcome::Paired { device, signature, .. }
            if *device == id && broker_signed(&challenge, &id, signature)),
        "{welcome:?}"
    );
    (device, id, stream)
}

/// The device's ticket for `space` on its connection.
fn device_join(stream: &UnixStream, space: &str) -> String {
    let joined = ask(
        stream,
        &SpaceOp::Join {
            space: space.to_owned(),
        },
    );
    if let SpaceResult::Admitted { ticket, .. } = joined {
        ticket
    } else {
        format!("not admitted: {joined:?}")
    }
}

#[test]
fn a_device_pairs_with_a_code_and_both_ends_authenticate_before_anything_moves() {
    let (_base, config) = config("pair");
    let me = agent("pair");
    let (space, ticket) = agent_space(&config, &me, now_secs());
    assert_eq!(
        op(&config, &me, put(&space, &ticket, "plan", "water at six")),
        SpaceResult::Stored
    );
    let code = pairing_code(&config, &me, &space, &ticket, now_secs());
    assert!(code.starts_with("pair-"), "{code}");
    let device = SigningKey::from_bytes(&[3; 32]);
    let (stream, challenge) = connect(&config);
    assert!(hatchling_space::is_nonce(&challenge.nonce), "{challenge:?}");
    let welcome = send_hello(&stream, &pair_hello(&device, &challenge, &code));
    let id = device_id(&device.verifying_key());
    let DeviceWelcome::Paired {
        device: paired_id,
        space: paired_into,
        signature,
    } = welcome
    else {
        panic_free_fail(&format!("{welcome:?}"));
        return;
    };
    assert_eq!(
        (paired_id.as_str(), paired_into.as_str()),
        (id.as_str(), space.as_str())
    );
    // The device checks the broker: its key, over the device's own nonce.
    assert!(broker_signed(&challenge, &id, &signature));
    assert!(!broker_signed(&challenge, "dev-other", &signature));
    // Authenticated, it joins the space and reads what the agent wrote.
    let device_ticket = device_join(&stream, &space);
    assert_eq!(
        ask(&stream, &get(&space, &device_ticket, "plan")),
        SpaceResult::Value {
            value: String::from("water at six")
        }
    );
    drop(stream);
    // A later connection authenticates with the key alone, and the broker
    // signs a fresh nonce under the same pinned key.
    let (again, challenge2) = connect(&config);
    assert_eq!(
        challenge2.broker_key, challenge.broker_key,
        "the broker key is stable"
    );
    assert_ne!(challenge2.nonce, challenge.nonce, "a fresh nonce each time");
    let welcome = send_hello(&again, &auth_hello(&device, &challenge2, &id));
    assert!(
        matches!(&welcome, DeviceWelcome::Authenticated { device, signature }
            if *device == id && broker_signed(&challenge2, &id, signature)),
        "{welcome:?}"
    );
    let ticket = device_join(&again, &space);
    assert!(ticket.starts_with("adm-"), "{ticket}");
    let store = SealedSpaceStore::new(&config.spaces.root, key());
    let record = store.get_device(&id).unwrap().unwrap();
    assert_eq!(record.spaces, std::slice::from_ref(&space));
    assert_eq!(record.key, b64(device.verifying_key().as_bytes()));
    // Paired into the same space again: still named once.
    let code = pairing_code(
        &config,
        &me,
        &space,
        &ticket_of(&config, &me, &space),
        now_secs(),
    );
    let (repaired, challenge3) = connect(&config);
    assert!(matches!(
        send_hello(&repaired, &pair_hello(&device, &challenge3, &code)),
        DeviceWelcome::Paired { .. }
    ));
    assert_eq!(store.get_device(&id).unwrap().unwrap().spaces, [space]);
}

/// A fresh agent ticket for `space`.
fn ticket_of(config: &Config, who: &SpaceSession, space: &str) -> String {
    let joined = op(
        config,
        who,
        SpaceOp::Join {
            space: space.to_owned(),
        },
    );
    if let SpaceResult::Admitted { ticket, .. } = joined {
        ticket
    } else {
        format!("not admitted: {joined:?}")
    }
}

/// Fails the calling test with `what`, without a `panic!`.
fn panic_free_fail(what: &str) {
    assert!(what.is_empty(), "{what}");
}

#[test]
fn a_used_or_expired_pairing_code_is_refused_and_the_refusal_logged() {
    let (_base, config) = config("replay");
    let me = agent("replay");
    let start = now_secs();
    let (space, ticket) = agent_space(&config, &me, start);
    let code = pairing_code(&config, &me, &space, &ticket, start);
    let _first = paired(&config, 4, &code);
    // Replayed, by another device with its own key.
    let thief = SigningKey::from_bytes(&[5; 32]);
    let (stream, challenge) = connect(&config);
    assert_eq!(
        send_hello(&stream, &pair_hello(&thief, &challenge, &code)),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::BadCode
        }
    );
    // Expired: issued so long ago that its lifetime has passed.
    let long_ago = start - hatchling_space::PAIRING_TTL_SECS - 60;
    let (old_space, old_ticket) = agent_space(&config, &me, long_ago);
    let stale = pairing_code(&config, &me, &old_space, &old_ticket, long_ago);
    let (stream, challenge) = connect(&config);
    assert_eq!(
        send_hello(&stream, &pair_hello(&thief, &challenge, &stale)),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::BadCode
        }
    );
    // Both refusals are logged, each as what it was.
    let logged: Vec<PairingRefusal> = pairing_refusals()
        .into_iter()
        .filter(|refusal| refusal.at >= start)
        .map(|refusal| refusal.reason)
        .collect();
    assert!(logged.contains(&PairingRefusal::Used), "{logged:?}");
    assert!(logged.contains(&PairingRefusal::Expired), "{logged:?}");
    // The thief is a member of nothing.
    let record = SealedSpaceStore::new(&config.spaces.root, key())
        .get(&space)
        .unwrap()
        .unwrap();
    let thief_id = device_id(&thief.verifying_key());
    assert!(!record.admits(&Principal::Device { id: thief_id }));
}

#[test]
fn an_unpaired_or_unproven_device_is_refused_before_any_state_moves() {
    let (_base, config) = config("unproven");
    let me = agent("unproven");
    let (space, ticket) = agent_space(&config, &me, now_secs());
    let code = pairing_code(&config, &me, &space, &ticket, now_secs());
    let device = SigningKey::from_bytes(&[6; 32]);
    let impostor = SigningKey::from_bytes(&[7; 32]);
    // A pair hello signed by another key: refused, and the code is not
    // spent by it.
    let (stream, challenge) = connect(&config);
    let DeviceHello::Pair {
        code: sent,
        device_key,
        nonce,
        ..
    } = pair_hello(&device, &challenge, &code)
    else {
        return;
    };
    let forged = impostor.sign(&pair_transcript(&challenge.nonce, &sent, &device_key));
    assert_eq!(
        send_hello(
            &stream,
            &DeviceHello::Pair {
                code: sent,
                device_key,
                nonce,
                signature: b64(&forged.to_bytes()),
            }
        ),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::BadSignature
        }
    );
    let (_, id, _stream) = paired(&config, 6, &code);
    // A device the broker never paired.
    let (stream, challenge) = connect(&config);
    let stranger = device_id(&impostor.verifying_key());
    assert_eq!(
        send_hello(&stream, &auth_hello(&impostor, &challenge, &stranger)),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::Unpaired
        }
    );
    // A paired id, proved with the wrong key, or over a stale nonce.
    let (stream, challenge) = connect(&config);
    assert_eq!(
        send_hello(&stream, &auth_hello(&impostor, &challenge, &id)),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::BadSignature
        }
    );
    let (stream, challenge) = connect(&config);
    let replayed = DeviceHello::Auth {
        device: id.clone(),
        nonce: device_nonce(),
        signature: b64(&device
            .sign(&auth_transcript(&"00".repeat(32), &id))
            .to_bytes()),
    };
    assert_ne!(challenge.nonce, "00".repeat(32));
    assert_eq!(
        send_hello(&stream, &replayed),
        DeviceWelcome::Refused {
            reason: DeviceRefusal::BadSignature
        }
    );
    // No hello at all, a space op instead: the connection ends unanswered,
    // and nothing is written.
    let (stream, _challenge) = connect(&config);
    write_frame(
        &stream,
        &serde_json::to_vec(&put(&space, "adm-none", "plan", "sneaked")).unwrap(),
    )
    .unwrap();
    assert!(
        read_frame(&stream).is_err(),
        "no reply to a skipped handshake"
    );
    assert_eq!(
        op(&config, &me, get(&space, &ticket, "plan")),
        refused(SpaceRefusal::NoSuchItem)
    );
}

#[test]
fn the_endpoint_serves_only_its_own_uid() {
    let (_base, config) = config("peer");
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let other = PeerUid::from_kernel(real_uid().wrapping_add(1));
    let outcome = std::thread::spawn(move || serve_device(&server, &config, key(), other));
    assert!(read_frame(&client).is_err(), "no challenge for another uid");
    assert!(matches!(
        outcome.join().unwrap(),
        Err(hatchling_broker::error::Error::PeerUnauthorized { .. })
    ));
}

#[test]
fn a_device_reaches_only_the_space_it_was_paired_into() {
    let (_base, config) = config("scope");
    let me = agent("scope");
    let (mine, my_ticket) = agent_space(&config, &me, now_secs());
    let (other, other_ticket) = agent_space(&config, &me, now_secs());
    assert_eq!(
        op(
            &config,
            &me,
            put(&other, &other_ticket, "secret", "not for devices")
        ),
        SpaceResult::Stored
    );
    let code = pairing_code(&config, &me, &mine, &my_ticket, now_secs());
    let (_, _, stream) = paired(&config, 8, &code);
    let ticket = device_join(&stream, &mine);
    // The other space: no join, and no sync through this space's ticket.
    assert_eq!(
        ask(
            &stream,
            &SpaceOp::Join {
                space: other.clone()
            }
        ),
        refused(SpaceRefusal::NoSuchSpace)
    );
    assert_eq!(
        ask(
            &stream,
            &SpaceOp::SyncPull {
                space: other,
                ticket: ticket.clone(),
                since: 0,
            }
        ),
        refused(SpaceRefusal::NotAdmitted)
    );
    // Nor anything only the agent may do.
    for denied in [
        SpaceOp::Create {
            name: String::from("mine now"),
            grants: Vec::new(),
        },
        SpaceOp::Pair {
            space: mine.clone(),
            ticket: ticket.clone(),
        },
        SpaceOp::AddTask {
            space: mine,
            ticket,
            task: String::from("t"),
            grants: Vec::new(),
            channel: String::from("local"),
        },
    ] {
        assert_eq!(ask(&stream, &denied), refused(SpaceRefusal::NotAllowed));
    }
}

/// The changes a pull returns, or what came back instead.
fn pulled(result: SpaceResult) -> (Vec<SyncItem>, u64, bool) {
    if let SpaceResult::Changes {
        changes,
        head,
        more,
    } = result
    {
        (changes, head, more)
    } else {
        (Vec::new(), 0, format!("{result:?}").is_empty())
    }
}

#[test]
fn a_pull_moves_only_the_named_spaces_changes_in_pages() {
    let (_base, config) = config("pull");
    let me = agent("pull");
    let (mine, my_ticket) = agent_space(&config, &me, now_secs());
    let (other, other_ticket) = agent_space(&config, &me, now_secs());
    assert_eq!(
        op(&config, &me, put(&other, &other_ticket, "elsewhere", "x")),
        SpaceResult::Stored
    );
    let code = pairing_code(&config, &me, &mine, &my_ticket, now_secs());
    let (_, _, stream) = paired(&config, 9, &code);
    let ticket = device_join(&stream, &mine);
    let pull = |since: u64| {
        pulled(ask(
            &stream,
            &SpaceOp::SyncPull {
                space: mine.clone(),
                ticket: ticket.clone(),
                since,
            },
        ))
    };
    assert_eq!(pull(0), (Vec::new(), 0, false), "nothing yet");
    for (key, value) in [("a", "1"), ("b", "2")] {
        assert_eq!(
            op(&config, &me, put(&mine, &my_ticket, key, value)),
            SpaceResult::Stored
        );
    }
    let (changes, head, more) = pull(0);
    assert!(!more);
    assert_eq!(head, 2);
    assert_eq!(
        changes,
        [
            SyncItem {
                key: String::from("a"),
                value: Some(String::from("1")),
                version: 1,
                writer: String::from("uid:1000"),
            },
            SyncItem {
                key: String::from("b"),
                value: Some(String::from("2")),
                version: 2,
                writer: String::from("uid:1000"),
            },
        ],
        "only this space's items"
    );
    // Since the head: only what changed after it, a removal as a tombstone.
    assert_eq!(
        op(
            &config,
            &me,
            SpaceOp::Remove {
                space: mine.clone(),
                ticket: my_ticket.clone(),
                key: String::from("a"),
            }
        ),
        SpaceResult::Removed
    );
    let (changes, head, _) = pull(2);
    assert_eq!(head, 3);
    assert_eq!(
        changes
            .iter()
            .map(|c| (c.key.as_str(), c.value.clone(), c.version))
            .collect::<Vec<_>>(),
        [("a", None, 3)]
    );
    // Many large items page; following the heads gets them all, once.
    let value = "v".repeat(1000);
    for n in 0..60 {
        let key = format!("big-{n}");
        assert_eq!(
            op(&config, &me, put(&mine, &my_ticket, &key, &value)),
            SpaceResult::Stored
        );
    }
    let (first, head, more) = pull(3);
    assert!(more, "a page holds fewer than all of them");
    assert!(!first.is_empty() && first.len() < 60, "{}", first.len());
    let mut seen = first.len();
    let mut since = head;
    let mut rest = more;
    // At most one page per item, so a pager that stops moving fails here
    // instead of running forever.
    for _ in 0..60 {
        let (page, head, more) = pull(since);
        seen += page.len();
        since = head;
        rest = more;
        if !rest {
            break;
        }
    }
    assert!(!rest, "the pull ends within sixty pages");
    assert_eq!(seen, 60);
}

/// A space the agent writes to, with a device paired into it and joined.
struct Synced {
    _base: Base,
    config: Config,
    me: SpaceSession,
    space: String,
    ticket: String,
    stream: UnixStream,
    device_ticket: String,
    device: String,
}

impl Synced {
    fn new(name: &str, seed: u8) -> Self {
        let (base, config) = config(name);
        let me = agent(name);
        let (space, ticket) = agent_space(&config, &me, now_secs());
        let code = pairing_code(&config, &me, &space, &ticket, now_secs());
        let (_, device, stream) = paired(&config, seed, &code);
        let device_ticket = device_join(&stream, &space);
        Self {
            _base: base,
            config,
            me,
            space,
            ticket,
            stream,
            device_ticket,
            device,
        }
    }

    /// One agent op on the space.
    fn agent(&self, op: SpaceOp) -> SpaceResult {
        self::op(&self.config, &self.me, op)
    }

    fn put(&self, key: &str, value: &str) -> SpaceResult {
        self.agent(put(&self.space, &self.ticket, key, value))
    }

    fn get(&self, key: &str) -> SpaceResult {
        self.agent(get(&self.space, &self.ticket, key))
    }

    fn remove(&self, key: &str) -> SpaceResult {
        self.agent(SpaceOp::Remove {
            space: self.space.clone(),
            ticket: self.ticket.clone(),
            key: key.to_owned(),
        })
    }

    /// A push from the device.
    fn push(&self, changes: &[(&str, Option<&str>, u64)]) -> SpaceResult {
        let changes = changes
            .iter()
            .map(|(key, value, base)| SyncChange {
                key: (*key).to_owned(),
                value: value.map(str::to_owned),
                base: *base,
            })
            .collect();
        ask(
            &self.stream,
            &SpaceOp::SyncPush {
                space: self.space.clone(),
                ticket: self.device_ticket.clone(),
                changes,
            },
        )
    }

    /// The conflict log after `after`, as the agent reads it.
    fn conflicts(&self, after: u64) -> (Vec<hatchling_core::ConflictEntry>, u64, bool) {
        let log = self.agent(SpaceOp::Conflicts {
            space: self.space.clone(),
            ticket: self.ticket.clone(),
            after,
        });
        if let SpaceResult::ConflictLog {
            entries,
            dropped,
            more,
        } = log
        {
            (entries, dropped, more)
        } else {
            (Vec::new(), u64::MAX, format!("{log:?}").is_empty())
        }
    }
}

const fn pushed(head: u64, conflicts: u32) -> SpaceResult {
    SpaceResult::Pushed { head, conflicts }
}

#[test]
fn last_writer_wins_and_the_overwritten_write_is_in_the_conflict_log() {
    let world = Synced::new("conflict", 10);
    assert_eq!(world.put("note", "first"), SpaceResult::Stored);
    // The device saw version 1; the agent writes version 2 meanwhile.
    assert_eq!(world.put("note", "agent edit"), SpaceResult::Stored);
    assert_eq!(
        world.push(&[("note", Some("device edit"), 1)]),
        pushed(3, 1)
    );
    // The last writer won...
    assert_eq!(
        world.get("note"),
        SpaceResult::Value {
            value: String::from("device edit")
        }
    );
    // ...and the loser is in the log, with what it lost.
    let (entries, dropped, more) = world.conflicts(0);
    assert_eq!((dropped, more), (0, false));
    assert_eq!(entries.len(), 1);
    let entry = entries.first().unwrap();
    assert_eq!(entry.seq, 1);
    assert_eq!(entry.key, "note");
    assert_eq!(entry.lost.as_deref(), Some("agent edit"));
    assert_eq!(
        (entry.lost_version, entry.lost_writer.as_str()),
        (2, "uid:1000")
    );
    assert_eq!(entry.winner, format!("device:{}", world.device));
    assert_eq!(entry.won_version, 3);
    assert!(world.conflicts(1).0.is_empty(), "nothing after it");
}

#[test]
fn a_push_over_what_it_saw_is_no_conflict_and_overwriting_a_removal_is_logged() {
    let world = Synced::new("no-conflict", 12);
    assert_eq!(world.put("note", "first"), SpaceResult::Stored);
    // Over the version it saw, and a new key: no conflicts.
    assert_eq!(
        world.push(&[("note", Some("again"), 1), ("fresh", Some("x"), 0)]),
        pushed(3, 0)
    );
    // The agent removes what the device wrote; the device writes it back
    // over a version it never saw. The log records that what was lost was
    // a removal.
    assert_eq!(world.remove("fresh"), SpaceResult::Removed);
    assert_eq!(world.push(&[("fresh", Some("back"), 3)]), pushed(5, 1));
    let (entries, _, _) = world.conflicts(0);
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.seq, e.key.as_str(), e.lost.clone(), e.lost_version))
            .collect::<Vec<_>>(),
        [(1, "fresh", None, 4)]
    );
    // A refused push changes nothing.
    assert_eq!(
        world.push(&[("ok", Some("1"), 0), ("Bad", Some("2"), 0)]),
        refused(SpaceRefusal::BadKey)
    );
    assert_eq!(world.get("ok"), refused(SpaceRefusal::NoSuchItem));
}

#[test]
fn the_conflict_log_is_bounded_and_says_how_much_it_dropped() {
    let (_base, config) = config("bound");
    let me = agent("bound");
    let (space, ticket) = agent_space(&config, &me, now_secs());
    let rounds = hatchling_space::MAX_CONFLICTS + 1;
    for n in 0..rounds {
        let value = format!("agent {n}");
        assert_eq!(
            op(&config, &me, put(&space, &ticket, "k", &value)),
            SpaceResult::Stored
        );
        // A writer that never saw any version: a conflict every time.
        let pushed = op(
            &config,
            &me,
            SpaceOp::SyncPush {
                space: space.clone(),
                ticket: ticket.clone(),
                changes: vec![SyncChange {
                    key: String::from("k"),
                    value: Some(format!("push {n}")),
                    base: 0,
                }],
            },
        );
        assert!(
            matches!(pushed, SpaceResult::Pushed { conflicts: 1, .. }),
            "{pushed:?}"
        );
    }
    let SpaceResult::ConflictLog {
        entries,
        dropped,
        more,
    } = op(
        &config,
        &me,
        SpaceOp::Conflicts {
            space: space.clone(),
            ticket,
            after: 0,
        },
    )
    else {
        return;
    };
    assert_eq!(dropped, 1);
    assert_eq!(entries.len(), hatchling_space::MAX_CONFLICTS);
    // Each kept entry holds its own lost value: what the agent wrote just
    // before that push.
    let lost: Vec<Option<String>> = entries.iter().map(|e| e.lost.clone()).collect();
    let expected: Vec<Option<String>> = (1..rounds).map(|n| Some(format!("agent {n}"))).collect();
    assert_eq!(lost, expected);
    assert_eq!(entries.first().map(|e| e.seq), Some(2));
    assert!(!more, "the whole log fits one page");
    // The dropped entry is gone from the store too, lost value and all.
    let store = SealedSpaceStore::new(&config.spaces.root, key());
    assert!(store.get_conflict(&space, 1).unwrap().is_none());
    assert!(store.get_conflict(&space, 2).unwrap().is_some());
}

/// A PNG's leading bytes, and some more.
fn png(len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(len, 7);
    bytes
}

fn put_media(space: &str, ticket: &str, kind: &str, data: &str) -> SpaceOp {
    SpaceOp::PutMedia {
        media_kind: kind.to_owned(),
        space: space.to_owned(),
        ticket: ticket.to_owned(),
        data: data.to_owned(),
    }
}

#[test]
fn media_is_typed_by_the_broker_and_served_with_its_headers() {
    let world = Synced::new("media", 11);
    // The largest media there is, uploaded and fetched through the device
    // endpoint's frames.
    let image = png(hatchling_space::MAX_MEDIA_BYTES);
    let stored = ask(
        &world.stream,
        &put_media(&world.space, &world.device_ticket, "image", &b64(&image)),
    );
    let SpaceResult::MediaStored {
        media,
        content_type,
        bytes,
    } = stored
    else {
        panic_free_fail(&format!("{stored:?}"));
        return;
    };
    assert!(media.starts_with("med-"), "{media}");
    assert_eq!((content_type.as_str(), bytes), ("image/png", 32768));
    let fetched = ask(
        &world.stream,
        &SpaceOp::GetMedia {
            space: world.space.clone(),
            ticket: world.device_ticket.clone(),
            media,
        },
    );
    let SpaceResult::Media {
        content_type,
        data,
        headers,
        ..
    } = fetched
    else {
        panic_free_fail(&format!("{fetched:?}"));
        return;
    };
    assert_eq!(content_type, "image/png");
    assert_eq!(data, b64(&image));
    let headers: Vec<(String, String)> = headers.into_iter().map(|h| (h.name, h.value)).collect();
    assert_eq!(
        headers,
        [
            (String::from("Content-Type"), String::from("image/png")),
            (
                String::from("X-Content-Type-Options"),
                String::from("nosniff")
            ),
            (
                String::from("Content-Disposition"),
                String::from("attachment")
            ),
            (
                String::from("Content-Security-Policy"),
                String::from("default-src 'none'; sandbox")
            ),
        ]
    );
}

#[test]
fn media_out_of_bounds_or_of_the_wrong_type_is_refused_before_admission() {
    let world = Synced::new("media-refused", 13);
    let kept = world.agent(put_media(
        &world.space,
        &world.ticket,
        "image",
        &b64(&png(64)),
    ));
    let SpaceResult::MediaStored { media, .. } = kept else {
        panic_free_fail(&format!("{kept:?}"));
        return;
    };
    // The wrong kind, script-bearing bytes, oversize, not base64.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>go()</script></svg>";
    for (kind, data, reason) in [
        ("audio", b64(&png(64)), SpaceRefusal::WrongMediaType),
        ("image", b64(svg), SpaceRefusal::WrongMediaType),
        (
            "image",
            b64(&png(hatchling_space::MAX_MEDIA_BYTES + 1)),
            SpaceRefusal::MediaTooLarge,
        ),
        ("image", String::from("%%%"), SpaceRefusal::BadMedia),
    ] {
        assert_eq!(
            world.agent(put_media(&world.space, &world.ticket, kind, &data)),
            refused(reason),
            "{kind} {reason:?}"
        );
    }
    let record = SealedSpaceStore::new(&world.config.spaces.root, key())
        .get(&world.space)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.media.as_slice(),
        std::slice::from_ref(&media),
        "none of the refused media is kept"
    );
    // Another space's media, and unknown media, are not served here.
    let (other, other_ticket) = agent_space(&world.config, &world.me, now_secs());
    for (in_space, with_ticket, id) in [
        (other.as_str(), other_ticket.as_str(), media.as_str()),
        (
            world.space.as_str(),
            world.ticket.as_str(),
            "med-nonexistent",
        ),
    ] {
        assert_eq!(
            world.agent(SpaceOp::GetMedia {
                space: in_space.to_owned(),
                ticket: with_ticket.to_owned(),
                media: id.to_owned(),
            }),
            refused(SpaceRefusal::NoSuchMedia)
        );
    }
}

#[test]
fn the_device_endpoint_refuses_to_serve_the_agents_own_uid() {
    let (_base, mut config) = config("open");
    let socket = format!("/tmp/hlde-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&socket);
    config.spaces.device_socket = Some(socket.parse().unwrap());
    // The agent's uid is the broker's own: the cell could pose as a device.
    let refused = hatchling_broker::open_device_endpoint(&config);
    assert!(matches!(
        refused,
        Err(hatchling_broker::error::Error::InvalidConfig {
            key: "HATCHLING_DEVICE_SOCKET",
            ..
        })
    ));
    assert!(!std::path::Path::new(&socket).exists(), "unlinked again");
    // Another agent uid: bound, owner-only.
    config.agent_uid = PeerUid::from_kernel(real_uid().wrapping_add(1));
    let endpoint = hatchling_broker::open_device_endpoint(&config)
        .unwrap()
        .unwrap();
    assert_eq!(endpoint.peer_uid(), PeerUid::from_kernel(real_uid()));
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&socket).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    drop(endpoint);
    let _ = std::fs::remove_file(&socket);
    // Bound directly: its one peer is the broker's own uid.
    let endpoint = hatchling_broker::DeviceEndpoint::bind(&socket.parse().unwrap()).unwrap();
    assert_eq!(endpoint.peer_uid(), PeerUid::from_kernel(real_uid()));
    drop(endpoint);
    let _ = std::fs::remove_file(&socket);
    // No device socket configured: no endpoint.
    config.spaces.device_socket = None;
    assert!(
        hatchling_broker::open_device_endpoint(&config)
            .unwrap()
            .is_none()
    );
}

#[test]
fn any_lost_value_fits_the_conflict_log_and_a_long_log_pages() {
    let world = Synced::new("conflict-pages", 14);
    // The largest value of control characters: its JSON escape is six
    // times its size, past one sealed entry, so the log keeps the lost
    // value raw beside the entry.
    let control = "\u{1}".repeat(hatchling_space::MAX_VALUE_BYTES);
    for n in 0..8 {
        assert_eq!(world.put("k", &control), SpaceResult::Stored, "{n}");
        assert!(
            matches!(
                world.push(&[("k", Some("push"), 0)]),
                SpaceResult::Pushed { conflicts: 1, .. }
            ),
            "{n}"
        );
    }
    // Eight entries of about 48 KiB each: one per page, in order.
    let mut after = 0;
    let mut seen = Vec::new();
    let mut more = true;
    // At most one page per entry, so a pager that stops moving fails here
    // instead of running forever.
    for _ in 0..8 {
        let (entries, _, rest) = world.conflicts(after);
        assert_eq!(entries.len(), 1, "one big entry per page");
        let entry = entries.first().unwrap();
        assert_eq!(entry.lost.as_deref(), Some(control.as_str()));
        seen.push(entry.seq);
        after = entry.seq;
        more = rest;
        if !more {
            break;
        }
    }
    assert!(!more, "the log ends within eight pages");
    assert_eq!(seen, (1..=8).collect::<Vec<u64>>());
}

#[test]
fn two_devices_pair_and_each_authenticates_by_its_own_key() {
    let world = Synced::new("two-devices", 15);
    let code = pairing_code(
        &world.config,
        &world.me,
        &world.space,
        &world.ticket,
        now_secs(),
    );
    let (_, second, _stream) = paired(&world.config, 16, &code);
    for (seed, id) in [(15u8, world.device.clone()), (16, second)] {
        let device = SigningKey::from_bytes(&[seed; 32]);
        let (stream, challenge) = connect(&world.config);
        let welcome = send_hello(&stream, &auth_hello(&device, &challenge, &id));
        assert!(
            matches!(&welcome, DeviceWelcome::Authenticated { device, .. } if *device == id),
            "{seed}: {welcome:?}"
        );
    }
}

/// A GIF's leading bytes, and some more.
fn gif(len: usize) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.resize(len, 3);
    bytes
}

#[test]
fn each_media_keeps_its_own_bytes_and_type_and_stays_in_its_space() {
    let world = Synced::new("media-two", 17);
    let store = |data: &[u8]| {
        let stored = world.agent(put_media(&world.space, &world.ticket, "image", &b64(data)));
        if let SpaceResult::MediaStored { media, .. } = stored {
            media
        } else {
            format!("not stored: {stored:?}")
        }
    };
    let png_id = store(&png(100));
    let gif_id = store(&gif(200));
    for (id, content_type, bytes) in [
        (&png_id, "image/png", png(100)),
        (&gif_id, "image/gif", gif(200)),
    ] {
        let fetched = world.agent(SpaceOp::GetMedia {
            space: world.space.clone(),
            ticket: world.ticket.clone(),
            media: id.clone(),
        });
        assert!(
            matches!(&fetched, SpaceResult::Media { media, content_type: ct, data, .. }
                if media == id && ct == content_type && *data == b64(&bytes)),
            "{id}: {fetched:?}"
        );
    }
    // Another space's media named in this space's record (a record gone
    // wrong) is still not served from here.
    let (other, other_ticket) = agent_space(&world.config, &world.me, now_secs());
    let foreign = world.agent(put_media(&other, &other_ticket, "image", &b64(&png(50))));
    let SpaceResult::MediaStored { media: foreign, .. } = foreign else {
        panic_free_fail(&format!("{foreign:?}"));
        return;
    };
    let spaces = SealedSpaceStore::new(&world.config.spaces.root, key());
    let mut record = spaces.get(&world.space).unwrap().unwrap();
    record.media.push(foreign.clone());
    assert!(spaces.put(&record).unwrap());
    assert_eq!(
        world.agent(SpaceOp::GetMedia {
            space: world.space.clone(),
            ticket: world.ticket.clone(),
            media: foreign,
        }),
        refused(SpaceRefusal::NoSuchMedia)
    );
}
