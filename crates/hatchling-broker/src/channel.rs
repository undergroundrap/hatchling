//! Broker-side channel mediation.
//!
//! The broker owns the channel endpoints. The cell-side agent never
//! touches a channel socket directly; it asks the broker to receive or
//! send via [`ChannelOp`](hatchling_core::ChannelOp), and the broker
//! drives the concrete [`Channel`](hatchling_channel::Channel)
//! implementation.
//!
//! Identity is broker-asserted: inbound messages are stamped with the
//! authenticated peer uid, and outbound messages are stamped before
//! delivery. The agent cannot spoof a sender.
//!
//! The confirmation-rail boundary is broker-classified too.
//! Without an approval, a send goes out only if it is a *conversational
//! reply*: it answers an inbound message the broker itself delivered on
//! that channel (and account) in this session, one reply per delivered
//! message, and the cell did not flag it state-changing. Everything
//! else — an unsolicited send, a second reply to one message, anything
//! flagged — is state-changing and needs a broker-minted approval
//! ([`crate::approval`]) for exactly that channel, account, and text.
//! The cell's `state_changing` flag can put a send on the rail but
//! never take one off, and the cell controls neither thing that
//! authorizes a send: it cannot deliver itself an inbound message, and
//! it cannot mint.
//!
//! Credentials are in broker custody (, [`crate::custody`]): a
//! credentialed channel is lent its account's secret per call, and the
//! cell names the account only by a handle this session minted.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use hatchling_channel::{
    Channel, ChannelId, Credential, LocalChannel, LocalHub, Message, SenderId,
};
use hatchling_core::{ChannelOp, ChannelResult};
use hatchling_vault::OpensStores;

use crate::approval::{ApprovalLedger, ApprovalToken, MintError, Verdict};
use crate::config::ChannelAccountSpec;
use crate::custody::AccountBook;
use crate::{Error, VaultConfig};

/// Most unanswered inbound messages one channel account banks as reply
/// credit.
///
/// Past it, further deliveries grant nothing more: a flood of inbound
/// messages cannot pre-authorize a flood of unapproved sends.
pub const MAX_REPLY_CREDITS: u32 = 16;

/// Where a message goes out: the channel, and the account's label on a
/// credentialed channel.
type Route = (String, Option<String>);

/// The broker's channel registry: channel id → owned endpoint.
///
/// Created per session from the configuration. The session driver
/// borrows it mutably per op; channels are not `Sync` (they own I/O
/// state), so the registry lives behind a session-local lock.
pub struct ChannelRegistry {
    channels: HashMap<String, Box<dyn Channel>>,
    /// The session's channel accounts and the secrets they hold.
    accounts: AccountBook,
    /// The confirmation-rail state: reply credit and approvals.
    rail: Rail,
}

impl std::fmt::Debug for ChannelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelRegistry")
            .field("channels", &self.channel_ids())
            .finish()
    }
}

impl ChannelRegistry {
    /// Build a session's registry from the enabled channel ids and the
    /// configured channel accounts.
    ///
    /// v0.1: the only concrete channel is the local channel, which is
    /// credential-less. Unknown ids are an error — the broker fails
    /// closed rather than running with a missing channel.
    ///
    /// The local channel is broker-owned and never blocks: with the
    /// ingress configured it is backed by the ingress `local_hub`
    ///, delivering through its output gate in the session's
    /// scope, otherwise it is an in-memory channel nothing feeds. A
    /// daemon must never block on stdin: the stdio variant of
    /// [`LocalChannel`] is for agent-side CLI testing, not for the
    /// broker.
    ///
    /// Account secrets are loaded here, from the sealed vault, into the
    /// session's [`AccountBook`]. The vault is opened only when an
    /// account is configured: a session without accounts touches no
    /// file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for an unknown channel id or a
    /// channel whose accounts do not fit it, and [`Error::Vault`] or
    /// [`Error::ChannelCustody`] if an account cannot be loaded.
    pub fn new(
        enabled: &[String],
        accounts: &[ChannelAccountSpec],
        vault: &VaultConfig,
        key: &dyn OpensStores,
        local_hub: Option<(&LocalHub, &str)>,
    ) -> Result<Self, Error> {
        let mut channels: HashMap<String, Box<dyn Channel>> = HashMap::new();
        for id in enabled {
            if id == "local" {
                let local = local_hub.map_or_else(LocalChannel::in_memory, |(hub, scope)| {
                    LocalChannel::with_hub(hub.clone(), scope.to_owned())
                });
                channels.insert(id.clone(), Box::new(local));
            } else {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_CHANNELS",
                    reason: format!("unknown channel: {id:?}"),
                });
            }
        }
        let book = if accounts.is_empty() {
            AccountBook::default()
        } else {
            let names: Vec<_> = accounts
                .iter()
                .map(|account| account.secret.clone())
                .collect();
            let secrets = crate::secrets::Secrets::new(&vault.root, key)
                .broker_read("channel-account", &names)?;
            AccountBook::load(accounts, secrets)?
        };
        Self::assemble(channels, book)
    }

    /// A registry over `channels` holding `accounts`, with no reply
    /// credit and no approvals.
    ///
    /// Custody fits exactly: every credentialed channel has an account,
    /// and every account is on a registered, credentialed channel — so
    /// no credential is lent to a channel that did not ask for one, and
    /// no credentialed channel is left unable to authenticate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] when channels and accounts do
    /// not fit.
    pub fn assemble(
        channels: HashMap<String, Box<dyn Channel>>,
        accounts: AccountBook,
    ) -> Result<Self, Error> {
        let misfit = |reason: String| Error::InvalidConfig {
            key: "HATCHLING_CHANNEL_ACCOUNTS",
            reason,
        };
        for channel in accounts.channels() {
            match channels.get(channel) {
                None => {
                    return Err(misfit(format!(
                        "accounts on an unknown channel: {channel:?}"
                    )));
                }
                Some(ch) if !ch.requires_credential() => {
                    return Err(misfit(format!(
                        "channel {channel:?} takes no credential, but has accounts"
                    )));
                }
                Some(_) => {}
            }
        }
        for (id, ch) in &channels {
            if ch.requires_credential() && !accounts.has_accounts(id) {
                return Err(misfit(format!(
                    "channel {id:?} needs a credential, but has no account"
                )));
            }
        }
        Ok(Self {
            channels,
            accounts,
            rail: Rail::default(),
        })
    }

    /// Mints a single-use rail approval for exactly `text` on `channel`
    /// as `account` (its label; `None` on a credential-less channel),
    /// valid on this session only.
    ///
    /// Broker-side only: this is the hook the confirmation rail
    /// calls once a person has approved the message. No
    /// session op reaches it, so a cell can never approve its own send.
    ///
    /// # Errors
    ///
    /// Returns [`MintError`] when the session's ledger is full or the
    /// RNG is unavailable.
    pub fn mint_approval(
        &mut self,
        channel: &str,
        account: Option<&str>,
        text: &str,
        now_secs: u64,
    ) -> Result<ApprovalToken, MintError> {
        self.rail.approvals.mint(channel, account, text, now_secs)
    }

    /// The ids of the registered channels.
    #[must_use]
    pub fn channel_ids(&self) -> Vec<String> {
        self.channels.keys().cloned().collect()
    }
}

/// A session's confirmation-rail state.
#[derive(Default)]
struct Rail {
    /// Per route: inbound messages delivered and not yet answered. Only
    /// a broker delivery grants one; a conversational reply spends one.
    reply_credits: HashMap<Route, u32>,
    /// This session's unspent rail approvals.
    approvals: ApprovalLedger,
}

impl Rail {
    /// The rail decision for one send. See the module docs for the rule.
    fn authorize_send(
        &mut self,
        route: &Route,
        text: &str,
        cell_flagged: bool,
        approval: Option<&str>,
        now_secs: u64,
    ) -> bool {
        let (channel, account) = (route.0.as_str(), route.1.as_deref());
        if let Some(token) = approval {
            // A presented approval must verify, whatever else holds: a
            // forged, spent, expired, or mismatched one is refused, not
            // ignored in favor of a reply credit.
            return match self
                .approvals
                .verify(token, channel, account, text, now_secs)
            {
                Verdict::Approved => true,
                verdict @ (Verdict::Unknown | Verdict::Expired | Verdict::Mismatch) => {
                    tracing::warn!(
                        channel,
                        ?verdict,
                        "channel send refused: approval did not verify"
                    );
                    false
                }
            };
        }
        if cell_flagged {
            // The cell may put a send on the rail; with no approval it
            // stays there.
            return false;
        }
        let credit = self.reply_credits.entry(route.clone()).or_insert(0);
        if *credit == 0 {
            tracing::warn!(
                channel,
                "channel send refused: not a reply, so it needs an approval"
            );
            return false;
        }
        *credit -= 1;
        true
    }

    /// Banks one reply credit for a message just delivered on `route`.
    fn grant_reply_credit(&mut self, route: Route) {
        let credit = self.reply_credits.entry(route).or_insert(0);
        *credit = credit.saturating_add(1).min(MAX_REPLY_CREDITS);
    }
}

/// Handle one [`ChannelOp`] from the authenticated peer.
///
/// The `peer_uid` is the kernel-reported uid from `SO_PEERCRED` — the
/// only identity signal. It stamps every message.
///
/// Checks run in a fixed order, and each refusal spends nothing the
/// later ones would: an unknown channel is `Denied`; then an account
/// handle that does not fit the channel is `UnknownAccount`; only then
/// does a send meet the rail.
///
/// # Errors
///
/// Currently infallible: protocol-level denials (unknown channel,
/// unknown account, missing rail approval) become replies, not errors
/// — the agent learns "no", not why — and a backend failure is the
/// opaque [`ChannelError`](ChannelResult::ChannelError).
pub fn handle_channel_op(
    registry: &mut ChannelRegistry,
    op: &ChannelOp,
    peer_uid: u32,
) -> Result<ChannelResult, Error> {
    Ok(match op {
        ChannelOp::Receive { channel, account } => {
            receive(registry, channel, account.as_deref(), peer_uid)
        }
        ChannelOp::Accounts { channel } => {
            if registry.channels.contains_key(channel) {
                ChannelResult::Accounts {
                    accounts: registry.accounts.listing(channel),
                }
            } else {
                ChannelResult::Denied
            }
        }
        ChannelOp::Send {
            channel,
            text,
            state_changing,
            approval,
            account,
        } => send(
            registry,
            &Outbound {
                channel,
                text,
                state_changing: *state_changing,
                approval: approval.as_deref(),
                account: account.as_deref(),
            },
            peer_uid,
        ),
    })
}

/// One send, as the cell asked for it.
struct Outbound<'a> {
    channel: &'a str,
    text: &'a str,
    state_changing: bool,
    approval: Option<&'a str>,
    account: Option<&'a str>,
}

/// Receives the next message on `channel` as the account `handle`.
fn receive(
    registry: &mut ChannelRegistry,
    channel: &str,
    handle: Option<&str>,
    peer_uid: u32,
) -> ChannelResult {
    let Some(ch) = registry.channels.get_mut(channel) else {
        // Unknown channel: denial, not an error. The agent learns "no".
        return ChannelResult::Denied;
    };
    let account = match registry
        .accounts
        .resolve(channel, ch.requires_credential(), handle)
    {
        Ok(account) => account,
        Err(rejection) => {
            tracing::warn!(channel, ?rejection, "channel receive refused: account");
            return ChannelResult::UnknownAccount;
        }
    };
    let credential = account.map(|account| Credential::new(account.secret()));
    // The channel backend may fail; that is opaque to the agent.
    let Ok(msg) = ch.receive(credential.as_ref()) else {
        return ChannelResult::ChannelError;
    };
    let Some(m) = msg else {
        return ChannelResult::NoMessage;
    };
    if registry.accounts.exposes_a_secret(m.text.as_bytes()) {
        // Custody backstop: a backend that put an account secret in
        // the text does not get it in front of the cell.
        tracing::warn!(
            channel,
            "inbound message withheld: its text carries account credential bytes"
        );
        return ChannelResult::ChannelError;
    }
    // A broker-delivered message opens one conversational turn: the
    // agent may answer it once, as the same account, without an
    // approval.
    let label = account.map(|account| account.label().to_owned());
    registry
        .rail
        .grant_reply_credit((channel.to_owned(), label));
    // Stamp the sender from the authenticated peer and the channel from
    // the registry key this op looked up. Whatever the channel backend
    // claimed about either is irrelevant: only the text is the
    // backend's.
    ChannelResult::Message {
        sender: stamp(peer_uid),
        channel: channel.to_owned(),
        text: m.text,
    }
}

/// Sends one message, if its account resolves and the rail allows it.
fn send(registry: &mut ChannelRegistry, out: &Outbound<'_>, peer_uid: u32) -> ChannelResult {
    let Some(ch) = registry.channels.get_mut(out.channel) else {
        return ChannelResult::Denied;
    };
    let account =
        match registry
            .accounts
            .resolve(out.channel, ch.requires_credential(), out.account)
        {
            Ok(account) => account,
            Err(rejection) => {
                tracing::warn!(
                    channel = out.channel,
                    ?rejection,
                    "channel send refused: account"
                );
                return ChannelResult::UnknownAccount;
            }
        };
    // Confirmation-rail boundary, broker-classified: a
    // conversational reply, or a broker-minted approval for exactly
    // this message. The cell's flag only tightens.
    let route = (
        out.channel.to_owned(),
        account.map(|account| account.label().to_owned()),
    );
    if !registry.rail.authorize_send(
        &route,
        out.text,
        out.state_changing,
        out.approval,
        now_secs(),
    ) {
        return ChannelResult::Denied;
    }
    let credential = account.map(|account| Credential::new(account.secret()));
    let msg = Message {
        sender: SenderId(stamp(peer_uid)),
        channel: ChannelId(out.channel.to_owned()),
        text: out.text.to_owned(),
    };
    match ch.send(msg, credential.as_ref()) {
        Ok(()) => ChannelResult::Sent,
        // The gate's notice went out in its place: the agent
        // learns the category and the event id, never more.
        Err(hatchling_channel::Error::Redacted { category, event_id }) => {
            ChannelResult::Redacted { category, event_id }
        }
        Err(_) => ChannelResult::ChannelError,
    }
}

/// The broker clock, for approval expiry.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The broker's sender stamp: `uid:<kernel-uid>`.
///
/// The only identity signal is the kernel-reported peer credential.
/// There is no other source.
fn stamp(peer_uid: u32) -> String {
    format!("uid:{peer_uid}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_vault::MasterKey;

    /// A vault config whose root does not exist: a registry without
    /// accounts must never open it.
    fn no_vault() -> (VaultConfig, MasterKey) {
        let root = std::path::PathBuf::from("/nonexistent/hatchling-no-vault");
        (
            VaultConfig {
                key_file: VaultConfig::default_key_file(&root),
                root,
                grants: Vec::new(),
            },
            MasterKey::generate().unwrap(),
        )
    }

    #[test]
    fn registry_lists_and_debugs_its_channel_ids() {
        let (vault, key) = no_vault();
        let registry =
            ChannelRegistry::new(&[String::from("local")], &[], &vault, &key, None).unwrap();
        assert_eq!(registry.channel_ids(), vec![String::from("local")]);
        assert_eq!(
            format!("{registry:?}"),
            r#"ChannelRegistry { channels: ["local"] }"#
        );
        assert!(
            ChannelRegistry::new(&[], &[], &vault, &key, None)
                .unwrap()
                .channel_ids()
                .is_empty()
        );
        assert!(!vault.root.exists(), "no accounts, no vault");
    }

    /// A test channel that returns a fixed message with a spoofed sender.
    /// Proves the broker overrides the channel's sender claim.
    struct SpoofChannel {
        message: Option<Message>,
    }

    impl Channel for SpoofChannel {
        fn channel_id(&self) -> ChannelId {
            ChannelId(String::from("spoof"))
        }

        fn receive(
            &mut self,
            _credential: Option<&Credential<'_>>,
        ) -> Result<Option<Message>, hatchling_channel::Error> {
            Ok(self.message.take())
        }

        fn send(
            &mut self,
            _msg: Message,
            _credential: Option<&Credential<'_>>,
        ) -> Result<(), hatchling_channel::Error> {
            Ok(())
        }
    }

    fn registry() -> ChannelRegistry {
        let mut reg = ChannelRegistry::assemble(HashMap::new(), AccountBook::default()).unwrap();
        reg.channels
            .insert(String::from("local"), Box::new(LocalChannel::in_memory()));
        reg
    }

    #[test]
    fn unknown_channel_id_fails_startup() {
        let (vault, key) = no_vault();
        let err =
            ChannelRegistry::new(&[String::from("telegram")], &[], &vault, &key, None).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_CHANNELS"));
    }

    #[test]
    fn receive_on_empty_channel_gives_no_message() {
        let mut reg = registry();
        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Receive {
                channel: String::from("local"),
                account: None,
            },
            1000,
        )
        .unwrap();
        assert_eq!(result, ChannelResult::NoMessage);
    }

    #[test]
    fn receive_stamps_sender_from_peer_uid_not_channel() {
        // The broker stamps the sender from the authenticated peer uid.
        // A channel backend claiming a different sender is overridden —
        // the agent never sees a self-declared identity.
        let mut reg = ChannelRegistry::assemble(HashMap::new(), AccountBook::default()).unwrap();
        reg.channels.insert(
            String::from("spoof"),
            Box::new(SpoofChannel {
                message: Some(Message {
                    sender: SenderId(String::from("attacker")),
                    channel: ChannelId(String::from("spoof")),
                    text: String::from("spoofed"),
                }),
            }),
        );

        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Receive {
                channel: String::from("spoof"),
                account: None,
            },
            1000,
        )
        .unwrap();
        // The sender is the broker's stamp (uid:1000), not "attacker".
        assert_eq!(
            result,
            ChannelResult::Message {
                sender: String::from("uid:1000"),
                channel: String::from("spoof"),
                text: String::from("spoofed"),
            }
        );
    }

    #[test]
    fn receive_stamps_channel_from_registry_key_not_backend() {
        // The channel id is broker-asserted too: a backend whose message
        // claims another channel is delivered under the registry key the
        // broker looked up, never under the claim.
        let mut reg = ChannelRegistry::assemble(HashMap::new(), AccountBook::default()).unwrap();
        reg.channels.insert(
            String::from("spoof"),
            Box::new(SpoofChannel {
                message: Some(Message {
                    sender: SenderId(String::from("attacker")),
                    channel: ChannelId(String::from("local")),
                    text: String::from("from elsewhere"),
                }),
            }),
        );

        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Receive {
                channel: String::from("spoof"),
                account: None,
            },
            1000,
        )
        .unwrap();
        assert_eq!(
            result,
            ChannelResult::Message {
                sender: String::from("uid:1000"),
                channel: String::from("spoof"),
                text: String::from("from elsewhere"),
            }
        );
    }

    #[test]
    fn receive_on_unknown_channel_is_denied() {
        let mut reg = registry();
        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Receive {
                channel: String::from("nope"),
                account: None,
            },
            1000,
        )
        .unwrap();
        assert_eq!(result, ChannelResult::Denied);
    }

    /// A registry whose local channel holds `inbound`, waiting.
    fn local_with(inbound: &[&str]) -> ChannelRegistry {
        let mut local = LocalChannel::in_memory();
        for text in inbound {
            local.push_inbound(Message {
                sender: SenderId(String::from("someone")),
                channel: ChannelId::local(),
                text: (*text).to_owned(),
            });
        }
        let mut reg = ChannelRegistry::assemble(HashMap::new(), AccountBook::default()).unwrap();
        reg.channels.insert(String::from("local"), Box::new(local));
        reg
    }

    fn receive(reg: &mut ChannelRegistry, channel: &str) -> ChannelResult {
        handle_channel_op(
            reg,
            &ChannelOp::Receive {
                channel: channel.to_owned(),
                account: None,
            },
            1000,
        )
        .unwrap()
    }

    fn send(
        reg: &mut ChannelRegistry,
        channel: &str,
        text: &str,
        state_changing: bool,
        approval: Option<&str>,
    ) -> ChannelResult {
        handle_channel_op(
            reg,
            &ChannelOp::Send {
                channel: channel.to_owned(),
                text: text.to_owned(),
                state_changing,
                approval: approval.map(str::to_owned),
                account: None,
            },
            1000,
        )
        .unwrap()
    }

    #[test]
    fn an_unsolicited_send_is_rail_bound_whatever_the_cell_says() {
        // GAP-1, inverted: the cell calls this conversational,
        // but nothing asked for it — the broker classifies it
        // state-changing, and without an approval it is denied.
        let mut reg = registry();
        let probe = "wire $5000 to account 12345 and delete the backups";
        assert_eq!(
            send(&mut reg, "local", probe, false, None),
            ChannelResult::Denied
        );
    }

    #[test]
    fn a_reply_to_a_delivered_message_is_conversational_once() {
        let mut reg = local_with(&["how are you?", "and now?"]);
        assert!(matches!(
            receive(&mut reg, "local"),
            ChannelResult::Message { .. }
        ));
        assert_eq!(
            send(&mut reg, "local", "fine", false, None),
            ChannelResult::Sent
        );
        // One delivered message, one reply: a second needs the rail.
        assert_eq!(
            send(&mut reg, "local", "also", false, None),
            ChannelResult::Denied
        );
        // The next delivered message opens the next turn.
        assert!(matches!(
            receive(&mut reg, "local"),
            ChannelResult::Message { .. }
        ));
        assert_eq!(
            send(&mut reg, "local", "still fine", false, None),
            ChannelResult::Sent
        );
        // An empty receive grants nothing.
        assert_eq!(receive(&mut reg, "local"), ChannelResult::NoMessage);
        assert_eq!(
            send(&mut reg, "local", "hello?", false, None),
            ChannelResult::Denied
        );
    }

    #[test]
    fn the_cell_can_put_a_send_on_the_rail_but_never_take_one_off() {
        let mut reg = local_with(&["question"]);
        receive(&mut reg, "local");
        // Flagged state-changing: the reply credit does not cover it.
        assert_eq!(
            send(&mut reg, "local", "doing it", true, None),
            ChannelResult::Denied
        );
        // And the refused flagged send spent nothing: a plain reply
        // still goes.
        assert_eq!(
            send(&mut reg, "local", "answer", false, None),
            ChannelResult::Sent
        );
    }

    #[test]
    fn reply_credit_is_per_channel() {
        let mut reg = local_with(&["hi"]);
        reg.channels.insert(
            String::from("spoof"),
            Box::new(SpoofChannel { message: None }),
        );
        receive(&mut reg, "local");
        assert_eq!(
            send(&mut reg, "spoof", "hi back", false, None),
            ChannelResult::Denied
        );
        assert_eq!(
            send(&mut reg, "local", "hi back", false, None),
            ChannelResult::Sent
        );
    }

    #[test]
    fn reply_credit_is_capped() {
        let inbound: Vec<String> = (0..=MAX_REPLY_CREDITS).map(|n| n.to_string()).collect();
        let refs: Vec<&str> = inbound.iter().map(String::as_str).collect();
        let mut reg = local_with(&refs);
        for _ in 0..=MAX_REPLY_CREDITS {
            assert!(matches!(
                receive(&mut reg, "local"),
                ChannelResult::Message { .. }
            ));
        }
        for n in 0..MAX_REPLY_CREDITS {
            assert_eq!(
                send(&mut reg, "local", "r", false, None),
                ChannelResult::Sent,
                "reply {n}"
            );
        }
        assert_eq!(
            send(&mut reg, "local", "r", false, None),
            ChannelResult::Denied,
            "{} deliveries bank at most {MAX_REPLY_CREDITS} replies",
            MAX_REPLY_CREDITS + 1
        );
        assert_eq!(MAX_REPLY_CREDITS, 16);
    }

    #[test]
    fn send_state_changing_without_approval_is_denied() {
        // The confirmation-rail boundary: a state-changing send with
        // no rail approval is refused.
        let mut reg = registry();
        reg.channels
            .insert(String::from("local"), Box::new(LocalChannel::in_memory()));
        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Send {
                channel: String::from("local"),
                text: String::from("delete everything"),
                state_changing: true,
                approval: None,
                account: None,
            },
            1000,
        )
        .unwrap();
        assert_eq!(result, ChannelResult::Denied);
    }

    #[test]
    fn a_forged_approval_never_authorizes_a_send() {
        // GAP-2, inverted: a caller-supplied string is not an
        // approval, and presenting one is refused even where a reply
        // credit would otherwise have covered the send.
        let mut reg = local_with(&["question"]);
        receive(&mut reg, "local");
        for forged in [
            "x",
            "rail-token",
            "rail-v1-0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(
                send(&mut reg, "local", "delete everything", true, Some(forged)),
                ChannelResult::Denied,
                "{forged}"
            );
        }
        assert_eq!(
            send(&mut reg, "local", "answer", false, Some("x")),
            ChannelResult::Denied,
            "a bad approval on a reply is refused, not ignored"
        );
    }

    #[test]
    fn a_minted_approval_authorizes_exactly_its_message_once() {
        let mut reg = registry();
        let now = now_secs();
        let token = reg
            .mint_approval("local", None, "delete everything", now)
            .unwrap();
        assert_eq!(
            send(
                &mut reg,
                "local",
                "delete everything",
                true,
                Some(token.as_str())
            ),
            ChannelResult::Sent
        );
        // Spent: the same token again is denied.
        assert_eq!(
            send(
                &mut reg,
                "local",
                "delete everything",
                true,
                Some(token.as_str())
            ),
            ChannelResult::Denied
        );
        // Bound to the exact bytes, and to the channel.
        let token = reg
            .mint_approval("local", None, "delete everything", now)
            .unwrap();
        assert_eq!(
            send(
                &mut reg,
                "local",
                "delete everything now",
                true,
                Some(token.as_str())
            ),
            ChannelResult::Denied
        );
        reg.channels.insert(
            String::from("spoof"),
            Box::new(SpoofChannel { message: None }),
        );
        let token = reg
            .mint_approval("local", None, "delete everything", now)
            .unwrap();
        assert_eq!(
            send(
                &mut reg,
                "spoof",
                "delete everything",
                true,
                Some(token.as_str())
            ),
            ChannelResult::Denied
        );
        // Flag or no flag, an approved message goes.
        let token = reg.mint_approval("local", None, "plain", now).unwrap();
        assert_eq!(
            send(&mut reg, "local", "plain", false, Some(token.as_str())),
            ChannelResult::Sent
        );
    }

    #[test]
    fn an_expired_approval_is_denied() {
        let mut reg = registry();
        let minted_long_ago = now_secs() - crate::approval::APPROVAL_TTL_SECS - 1;
        let token = reg
            .mint_approval("local", None, "late", minted_long_ago)
            .unwrap();
        assert_eq!(
            send(&mut reg, "local", "late", true, Some(token.as_str())),
            ChannelResult::Denied
        );
    }

    #[test]
    fn a_send_on_an_unknown_channel_spends_no_approval() {
        let mut reg = registry();
        let token = reg.mint_approval("local", None, "t", now_secs()).unwrap();
        assert_eq!(
            send(&mut reg, "nope", "t", true, Some(token.as_str())),
            ChannelResult::Denied
        );
        assert_eq!(
            send(&mut reg, "local", "t", true, Some(token.as_str())),
            ChannelResult::Sent,
            "the refused unknown-channel send left the token unspent"
        );
    }

    #[test]
    fn send_on_unknown_channel_is_denied() {
        let mut reg = registry();
        let result = handle_channel_op(
            &mut reg,
            &ChannelOp::Send {
                channel: String::from("nope"),
                text: String::from("hi"),
                state_changing: false,
                approval: None,
                account: None,
            },
            1000,
        )
        .unwrap();
        assert_eq!(result, ChannelResult::Denied);
    }

    /// the session's local channel is backed by the ingress hub.
    /// An injected message is delivered with the broker's stamps, whatever
    /// the ingress claimed, and it opens a conversational turn whose reply
    /// goes back to the ingress client.
    #[test]
    fn the_ingress_hub_feeds_the_local_channel_under_the_broker_stamps() {
        let (vault, key) = no_vault();
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let mut reg = ChannelRegistry::new(
            &[String::from("local")],
            &[],
            &vault,
            &key,
            Some((&hub, "session:t")),
        )
        .unwrap();
        assert_eq!(receive(&mut reg, "local"), ChannelResult::NoMessage);
        let claimed = Message {
            sender: SenderId(String::from("root")),
            channel: ChannelId(String::from("telegram")),
            text: String::from("from the terminal"),
        };
        assert_eq!(hub.inject(claimed), hatchling_channel::IngressEvent::Queued);
        assert_eq!(
            receive(&mut reg, "local"),
            ChannelResult::Message {
                sender: String::from("uid:1000"),
                channel: String::from("local"),
                text: String::from("from the terminal"),
            }
        );
        let replies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_replies = std::sync::Arc::clone(&replies);
        let _ = hub.attach(Box::new(move |m: &hatchling_channel::Cleared| {
            sink_replies.lock().unwrap().push(m.message().clone());
            Ok(())
        }));
        assert_eq!(
            send(&mut reg, "local", "got it", false, None),
            ChannelResult::Sent
        );
        let replies = replies.lock().unwrap().clone();
        assert_eq!(
            replies,
            [Message {
                sender: SenderId(String::from("uid:1000")),
                channel: ChannelId::local(),
                text: String::from("got it"),
            }]
        );
        // One delivery, one reply: the rail still holds.
        assert_eq!(
            send(&mut reg, "local", "and more", false, None),
            ChannelResult::Denied
        );
    }

    #[test]
    fn stamp_formats_uid() {
        assert_eq!(stamp(1000), "uid:1000");
        assert_eq!(stamp(0), "uid:0");
    }
}
