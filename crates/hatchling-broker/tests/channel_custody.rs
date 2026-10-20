//! Component: channel credential custody through the channel ops.
//!
//! The local channel is credential-less, so custody is exercised by a
//! test-only credentialed channel ([`KeyedChannel`]) whose "server"
//! authenticates every call against the account keys it knows. The
//! accounts are real: their secrets are sealed in a vault on disk and
//! loaded through [`AccountBook::load`], exactly as a session would.
//!
//! Every test in this binary logs into one process-wide capture
//! ([`logs`]), installed as the global default before any test emits:
//! a thread-scoped subscriber can miss events while other tests run in
//! parallel, and the no-key-bytes walk must see every line.
#![allow(
    clippy::unwrap_used,
    reason = "test fixtures: a failed vault or registry setup must fail the calling test loudly"
)]

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hatchling_channel::{Channel, ChannelId, Credential, LocalChannel, Message, SenderId};
use hatchling_core::{ChannelOp, ChannelResult, MAX_FRAME_BYTES, SecretName, SessionResponse};
use hatchling_vault::{MasterKey, SealedVault, Secret};

use hatchling_broker::custody::HANDLE_PREFIX;
use hatchling_broker::{
    AccountBook, ChannelAccountSpec, ChannelRegistry, Error, VaultConfig, handle_channel_op,
};

const WORK_KEY: &[u8] = b"sk-work-7f3a9c1e5b2d8046";
const HOME_KEY: &[u8] = b"sk-home-19e2c4a6b8d0f357";
const PEER: u32 = 1000;

/// What the keyed backend observed.
#[derive(Default)]
struct Backend {
    /// Every credential a call was lent, in order.
    lent: Vec<Vec<u8>>,
    /// Every outbound message: the key it was sent with, and its text.
    delivered: Vec<(Vec<u8>, String)>,
}

/// A credentialed channel, like a real integration: each call must
/// carry the key of an account the server knows, and each account has
/// its own inbox.
struct KeyedChannel {
    inboxes: HashMap<Vec<u8>, VecDeque<String>>,
    seen: Arc<Mutex<Backend>>,
    /// A hostile backend: echoes the key it was lent into the text.
    echo_key: bool,
    /// A gated backend: reports each send redacted.
    redact: bool,
}

impl KeyedChannel {
    fn new(seen: &Arc<Mutex<Backend>>, work: &[&str], home: &[&str]) -> Self {
        let inbox = |texts: &[&str]| texts.iter().map(|t| (*t).to_owned()).collect();
        Self {
            inboxes: HashMap::from([
                (WORK_KEY.to_vec(), inbox(work)),
                (HOME_KEY.to_vec(), inbox(home)),
            ]),
            seen: Arc::clone(seen),
            echo_key: false,
            redact: false,
        }
    }

    fn authenticate(
        &self,
        credential: Option<&Credential<'_>>,
    ) -> Result<Vec<u8>, hatchling_channel::Error> {
        let key = credential
            .map(|credential| credential.expose().to_vec())
            .ok_or_else(|| hatchling_channel::Error::Backend(String::from("no credential")))?;
        if !self.inboxes.contains_key(&key) {
            return Err(hatchling_channel::Error::Backend(String::from(
                "unknown credential",
            )));
        }
        self.seen.lock().unwrap().lent.push(key.clone());
        Ok(key)
    }
}

impl Channel for KeyedChannel {
    fn channel_id(&self) -> ChannelId {
        ChannelId(String::from("mail"))
    }

    fn requires_credential(&self) -> bool {
        true
    }

    fn receive(
        &mut self,
        credential: Option<&Credential<'_>>,
    ) -> Result<Option<Message>, hatchling_channel::Error> {
        let key = self.authenticate(credential)?;
        let text = self.inboxes.get_mut(&key).and_then(VecDeque::pop_front);
        let echo = self
            .echo_key
            .then(|| String::from_utf8_lossy(&key).into_owned());
        Ok(text.map(|text| Message {
            sender: SenderId(String::from("mail-server")),
            channel: ChannelId(String::from("mail")),
            text: match echo {
                Some(key) => format!("{text} [{key}]"),
                None => text,
            },
        }))
    }

    fn send(
        &mut self,
        msg: Message,
        credential: Option<&Credential<'_>>,
    ) -> Result<(), hatchling_channel::Error> {
        let key = self.authenticate(credential)?;
        if self.redact {
            return Err(hatchling_channel::Error::Redacted {
                category: String::from("secret"),
                event_id: String::from("red-1-2-3"),
            });
        }
        self.seen.lock().unwrap().delivered.push((key, msg.text));
        Ok(())
    }
}

/// A sealed vault on disk holding both account secrets.
struct Vault {
    config: VaultConfig,
    key: MasterKey,
}

impl Vault {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-custody-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let sealed = SealedVault::open(&root, &key).unwrap();
        for (name, secret) in [("mail-work", WORK_KEY), ("mail-home", HOME_KEY)] {
            sealed
                .store(
                    &name.parse::<SecretName>().unwrap(),
                    &Secret::new(secret.to_vec()).unwrap(),
                )
                .unwrap();
        }
        Self {
            config: VaultConfig {
                key_file: VaultConfig::default_key_file(&root),
                root,
                grants: Vec::new(),
            },
            key,
        }
    }

    fn root(&self) -> PathBuf {
        self.config.root.clone()
    }

    /// The configured accounts, loaded from this vault.
    fn book(&self, specs: &[(&str, &str, &str)]) -> Result<AccountBook, Error> {
        let specs: Vec<ChannelAccountSpec> = specs
            .iter()
            .map(|(channel, label, secret)| ChannelAccountSpec {
                channel: (*channel).to_owned(),
                label: (*label).to_owned(),
                secret: secret.parse().unwrap(),
            })
            .collect();
        let sealed = SealedVault::open(&self.config.root, &self.key).unwrap();
        let secrets = specs
            .iter()
            .map(|spec| sealed.fetch(&spec.secret).ok())
            .collect();
        AccountBook::load(&specs, secrets)
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root());
    }
}

const ACCOUNTS: [(&str, &str, &str); 2] =
    [("mail", "work", "mail-work"), ("mail", "home", "mail-home")];

/// A session: the local channel plus the keyed `mail` channel, with
/// both accounts loaded from the vault.
fn session(vault: &Vault, mail: KeyedChannel) -> ChannelRegistry {
    let mut channels: HashMap<String, Box<dyn Channel>> = HashMap::new();
    channels.insert(String::from("local"), Box::new(LocalChannel::in_memory()));
    channels.insert(String::from("mail"), Box::new(mail));
    ChannelRegistry::assemble(channels, vault.book(&ACCOUNTS).unwrap()).unwrap()
}

fn op(reg: &mut ChannelRegistry, op: &ChannelOp) -> ChannelResult {
    logs();
    handle_channel_op(reg, op, PEER).unwrap()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A reply as the session writes it on the wire.
fn wire(result: ChannelResult) -> Vec<u8> {
    serde_json::to_vec(&SessionResponse::Channel(result)).unwrap()
}

fn accounts(reg: &mut ChannelRegistry, channel: &str) -> ChannelResult {
    op(
        reg,
        &ChannelOp::Accounts {
            channel: channel.to_owned(),
        },
    )
}

fn receive(reg: &mut ChannelRegistry, channel: &str, account: Option<&str>) -> ChannelResult {
    op(
        reg,
        &ChannelOp::Receive {
            channel: channel.to_owned(),
            account: account.map(str::to_owned),
        },
    )
}

fn send(
    reg: &mut ChannelRegistry,
    account: Option<&str>,
    text: &str,
    approval: Option<&str>,
) -> ChannelResult {
    op(
        reg,
        &ChannelOp::Send {
            channel: String::from("mail"),
            text: text.to_owned(),
            state_changing: false,
            approval: approval.map(str::to_owned),
            account: account.map(str::to_owned),
        },
    )
}

/// The `work` and `home` handles, from the cell's own `Accounts` reply.
fn handles(reg: &mut ChannelRegistry) -> (String, String) {
    let ChannelResult::Accounts { accounts: listed } = accounts(reg, "mail") else {
        return (String::new(), String::new());
    };
    let find = |label: &str| {
        listed
            .iter()
            .find(|account| account.label == label)
            .map(|account| account.handle.clone())
            .unwrap_or_default()
    };
    (find("work"), find("home"))
}

#[test]
fn a_credentialed_round_trip_shows_the_cell_only_handles_and_results() {
    let vault = Vault::new("round-trip");
    let seen = Arc::new(Mutex::new(Backend::default()));
    let mut reg = session(&vault, KeyedChannel::new(&seen, &["hello work"], &[]));

    let listed = match accounts(&mut reg, "mail") {
        ChannelResult::Accounts { accounts } => accounts,
        ChannelResult::Message { .. }
        | ChannelResult::NoMessage
        | ChannelResult::Sent
        | ChannelResult::Denied
        | ChannelResult::ChannelError
        | ChannelResult::UnknownAccount
        | ChannelResult::Redacted { .. } => Vec::new(),
    };
    let labels: Vec<&str> = listed.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, ["home", "work"]);
    let (work, home) = handles(&mut reg);
    assert!(work.starts_with(HANDLE_PREFIX), "{work}");
    assert_ne!(work, home);

    assert_eq!(
        receive(&mut reg, "mail", Some(&work)),
        ChannelResult::Message {
            sender: String::from("uid:1000"),
            channel: String::from("mail"),
            text: String::from("hello work"),
        }
    );
    // The reply credit is the work account's: a reply as home is not a
    // reply, so it needs the rail.
    assert_eq!(
        send(&mut reg, Some(&home), "from home", None),
        ChannelResult::Denied
    );
    assert_eq!(
        send(&mut reg, Some(&work), "reply", None),
        ChannelResult::Sent
    );

    // The backend authenticated with the work key on both calls, and
    // the home key was never lent: the refused send reached no backend.
    assert_eq!(
        seen.lock().unwrap().lent,
        [WORK_KEY.to_vec(), WORK_KEY.to_vec()]
    );
    assert_eq!(
        seen.lock().unwrap().delivered,
        [(WORK_KEY.to_vec(), String::from("reply"))]
    );
}

#[test]
fn forged_and_misfit_handles_are_unknown_accounts_and_spend_nothing() {
    let vault = Vault::new("handles");
    let seen = Arc::new(Mutex::new(Backend::default()));
    let mut reg = session(&vault, KeyedChannel::new(&seen, &["m"], &[]));
    let (work, _) = handles(&mut reg);
    let foreign = {
        let other_seen = Arc::new(Mutex::new(Backend::default()));
        let mut other = session(&vault, KeyedChannel::new(&other_seen, &[], &[]));
        handles(&mut other).0
    };
    assert_ne!(foreign, work, "each session mints its own handles");

    let forged = format!("{}{}", HANDLE_PREFIX, "0".repeat(64));
    for bad in [forged.as_str(), "work", "mail-work", foreign.as_str()] {
        assert_eq!(
            receive(&mut reg, "mail", Some(bad)),
            ChannelResult::UnknownAccount,
            "{bad}"
        );
        assert_eq!(
            send(&mut reg, Some(bad), "t", None),
            ChannelResult::UnknownAccount,
            "{bad}"
        );
    }
    assert_eq!(
        receive(&mut reg, "mail", None),
        ChannelResult::UnknownAccount,
        "a credentialed channel needs an account"
    );
    assert_eq!(
        receive(&mut reg, "local", Some(&work)),
        ChannelResult::UnknownAccount,
        "a credential-less channel takes none"
    );
    assert_eq!(
        receive(&mut reg, "nope", Some(&work)),
        ChannelResult::Denied,
        "an unknown channel is refused before its account is looked at"
    );
    assert!(
        seen.lock().unwrap().lent.is_empty(),
        "no refused op reached the backend"
    );

    // A refused handle spends no approval: the same token then goes.
    let token = reg
        .mint_approval("mail", Some("work"), "approved", now_secs())
        .unwrap();
    assert_eq!(
        send(&mut reg, Some(&forged), "approved", Some(token.as_str())),
        ChannelResult::UnknownAccount
    );
    assert_eq!(
        send(&mut reg, Some(&work), "approved", Some(token.as_str())),
        ChannelResult::Sent
    );
}

#[test]
fn an_approval_is_bound_to_its_account() {
    let vault = Vault::new("approval");
    let seen = Arc::new(Mutex::new(Backend::default()));
    let mut reg = session(&vault, KeyedChannel::new(&seen, &[], &[]));
    let (work, home) = handles(&mut reg);
    let now = now_secs();
    let token = reg
        .mint_approval("mail", Some("work"), "pay rent", now)
        .unwrap();
    assert_eq!(
        send(&mut reg, Some(&home), "pay rent", Some(token.as_str())),
        ChannelResult::Denied,
        "approved as work, presented as home"
    );
    let token = reg
        .mint_approval("mail", Some("work"), "pay rent", now)
        .unwrap();
    assert_eq!(
        send(&mut reg, Some(&work), "pay rent", Some(token.as_str())),
        ChannelResult::Sent
    );
    assert_eq!(
        seen.lock().unwrap().delivered,
        [(WORK_KEY.to_vec(), String::from("pay rent"))]
    );
}

#[test]
fn a_backend_that_echoes_its_key_is_withheld() {
    let vault = Vault::new("echo");
    let seen = Arc::new(Mutex::new(Backend::default()));
    let mut mail = KeyedChannel::new(&seen, &["here is my key"], &[]);
    mail.echo_key = true;
    let mut reg = session(&vault, mail);
    let (work, _) = handles(&mut reg);
    assert_eq!(
        receive(&mut reg, "mail", Some(&work)),
        ChannelResult::ChannelError
    );
    assert_eq!(
        send(&mut reg, Some(&work), "reply", None),
        ChannelResult::Denied,
        "a withheld message grants no reply credit"
    );
}

/// The wire tag of every [`ChannelResult`] variant. Exhaustive: a new
/// variant fails to compile here until the walk below covers it.
const fn tag(result: &ChannelResult) -> &'static str {
    match result {
        ChannelResult::Message { .. } => "message",
        ChannelResult::NoMessage => "no-message",
        ChannelResult::Sent => "sent",
        ChannelResult::Denied => "denied",
        ChannelResult::ChannelError => "channel-error",
        ChannelResult::Accounts { .. } => "accounts",
        ChannelResult::UnknownAccount => "unknown-account",
        ChannelResult::Redacted { .. } => "redacted",
    }
}

/// Every form the key bytes could leak in: raw, hex, and base64.
fn leaked_forms(key: &[u8]) -> Vec<Vec<u8>> {
    let hex = key.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    vec![
        key.to_vec(),
        hex.into_bytes(),
        base64::engine::general_purpose::STANDARD
            .encode(key)
            .into_bytes(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(key)
            .into_bytes(),
    ]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A `tracing` writer into a shared buffer.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

/// The process-wide log capture, installed as the global default on
/// first use. Every broker call in this binary goes through [`op`],
/// which installs it first.
fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    Arc::clone(LOGS.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(Mutex::new(Capture(Arc::clone(&buffer))))
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        buffer
    }))
}

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One session that drives every reply a cell can reach on a
/// credentialed channel, returning the replies in order.
fn walk(vault: &Vault) -> Vec<ChannelResult> {
    let seen = Arc::new(Mutex::new(Backend::default()));
    let mut reg = session(vault, KeyedChannel::new(&seen, &["one"], &[]));
    let mut replies = Vec::new();
    replies.push(accounts(&mut reg, "mail")); // accounts
    replies.push(accounts(&mut reg, "local")); // accounts, empty
    let (work, home) = handles(&mut reg);
    replies.push(receive(&mut reg, "mail", Some(&work))); // message
    replies.push(send(&mut reg, Some(&work), "reply", None)); // sent
    replies.push(send(&mut reg, Some(&work), "again", None)); // denied
    replies.push(receive(&mut reg, "mail", Some(&work))); // no-message
    replies.push(receive(&mut reg, "mail", Some(&home))); // no-message
    replies.push(receive(&mut reg, "mail", Some("acct-v1-forged"))); // unknown-account
    replies.push(receive(&mut reg, "mail", None)); // unknown-account
    replies.push(accounts(&mut reg, "nope")); // denied
    let echo_seen = Arc::new(Mutex::new(Backend::default()));
    let mut echo = KeyedChannel::new(&echo_seen, &["echo"], &[]);
    echo.echo_key = true;
    let mut hostile = session(vault, echo);
    let (hostile_work, _) = handles(&mut hostile);
    replies.push(receive(&mut hostile, "mail", Some(&hostile_work))); // channel-error
    let gated_seen = Arc::new(Mutex::new(Backend::default()));
    let mut gated = KeyedChannel::new(&gated_seen, &["two"], &[]);
    gated.redact = true;
    let mut gated = session(vault, gated);
    let (gated_work, _) = handles(&mut gated);
    let _ = receive(&mut gated, "mail", Some(&gated_work));
    replies.push(send(&mut gated, Some(&gated_work), "reply", None)); // redacted
    // The registries' own debug forms are cell-invisible, but must not
    // carry keys either (a log line could format them).
    tracing::info!(registry = ?reg, hostile = ?hostile, "walk done");
    replies
}

#[test]
fn no_cell_reachable_reply_carries_key_bytes() {
    // The walked surface: every `ChannelResult` a cell can receive on a
    // credentialed channel, serialized exactly as the session writes it
    // (`SessionResponse::Channel` through `encode_reply`), plus every
    // log line the broker emits while serving them.
    let vault = Vault::new("walk");
    let replies = walk(&vault);

    let tags: std::collections::BTreeSet<&str> = replies.iter().map(tag).collect();
    assert_eq!(
        tags.into_iter().collect::<Vec<_>>(),
        [
            "accounts",
            "channel-error",
            "denied",
            "message",
            "no-message",
            "redacted",
            "sent",
            "unknown-account"
        ],
        "the walk reaches every reply variant"
    );
    let logs = logs().lock().unwrap().clone();
    assert!(
        contains(&logs, b"inbound message withheld"),
        "the capture saw the broker's logs"
    );
    let forms: Vec<Vec<u8>> = [WORK_KEY, HOME_KEY]
        .iter()
        .flat_map(|key| leaked_forms(key))
        .collect();
    for reply in replies {
        let bytes = wire(reply);
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        for form in &forms {
            assert!(
                !contains(&bytes, form),
                "key bytes in a reply: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
    for form in &forms {
        assert!(!contains(&logs, form), "key bytes in the broker's logs");
    }
    // The scan is live: a reply that did carry a key, in any form, is
    // caught by it.
    for form in &forms {
        let text = format!("x{}y", String::from_utf8_lossy(form));
        let leaky = wire(ChannelResult::Message {
            sender: String::new(),
            channel: String::new(),
            text,
        });
        assert!(contains(&leaky, form));
    }
}

#[test]
fn custody_must_fit_the_channels() {
    let vault = Vault::new("fit");
    let seen = Arc::new(Mutex::new(Backend::default()));
    let misfit = |channels: HashMap<String, Box<dyn Channel>>, book: AccountBook| {
        match ChannelRegistry::assemble(channels, book) {
            Err(Error::InvalidConfig {
                key: "HATCHLING_CHANNEL_ACCOUNTS",
                reason,
            }) => reason,
            Err(other) => format!("unexpected error: {other:?}"),
            Ok(_) => String::from("assembled"),
        }
    };
    let keyed = || {
        let mut channels: HashMap<String, Box<dyn Channel>> = HashMap::new();
        channels.insert(
            String::from("mail"),
            Box::new(KeyedChannel::new(&seen, &[], &[])),
        );
        channels
    };
    let local = || {
        let mut channels: HashMap<String, Box<dyn Channel>> = HashMap::new();
        channels.insert(String::from("local"), Box::new(LocalChannel::in_memory()));
        channels
    };
    assert!(misfit(keyed(), AccountBook::default()).contains("has no account"));
    assert!(
        misfit(
            local(),
            vault.book(&[("local", "work", "mail-work")]).unwrap()
        )
        .contains("takes no credential")
    );
    assert!(misfit(local(), vault.book(&ACCOUNTS).unwrap()).contains("unknown channel"));
    assert!(ChannelRegistry::assemble(keyed(), vault.book(&ACCOUNTS).unwrap()).is_ok());
    assert!(ChannelRegistry::assemble(local(), AccountBook::default()).is_ok());
}

#[test]
fn registry_construction_loads_accounts_from_the_vault() {
    let vault = Vault::new("new");
    let local = [String::from("local")];
    let spec = |secret: &str| ChannelAccountSpec {
        channel: String::from("local"),
        label: String::from("work"),
        secret: secret.parse().unwrap(),
    };
    // A secret the vault does not hold fails the session.
    let err = ChannelRegistry::new(
        &local,
        &[spec("mail-gone")],
        &vault.config,
        &vault.key,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Vault {
                source: hatchling_vault::Error::NotFound { .. }
            }
        ),
        "{err:?}"
    );
    // A held secret loads — and the fit check then refuses it, because
    // the local channel takes no credential.
    let err = ChannelRegistry::new(
        &local,
        &[spec("mail-work")],
        &vault.config,
        &vault.key,
        None,
    )
    .unwrap_err();
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
    // A key that cannot open the vault fails the session too.
    let wrong = MasterKey::generate().unwrap();
    assert!(
        ChannelRegistry::new(&local, &[spec("mail-work")], &vault.config, &wrong, None).is_err()
    );
}
