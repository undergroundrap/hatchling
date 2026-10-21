//! Agent-side channel client: messaging through the broker session.
//!
//! The agent never touches a channel socket. It sends [`ChannelOp`]s
//! over the peer-authorized session; the broker owns the endpoints,
//! stamps identities, and enforces the confirmation-rail boundary.
//!
//! Inbound messages arrive with broker-asserted sender identity. The
//! agent cannot spoof a sender, and it never sees a self-declared
//! identity.
//!
//! A credentialed channel is used as one of its accounts: the
//! client learns the account handles from [`ChannelClient::accounts`]
//! and binds one with [`ChannelClient::with_account`]. The handle is
//! all the cell ever holds; the account's secret stays with the broker.

use std::os::unix::net::UnixStream;

use hatchling_channel::{ChannelId, Message, SenderId};
use hatchling_core::{
    ChannelAccount, ChannelOp, ChannelResult, SessionRequest, SessionResponse,
    framing::{read_frame, write_frame},
};

use crate::Error;

/// A broker-mediated channel handle: send and receive through the session.
///
/// The `stream` is the peer-authorized broker session. Every op is one
/// request → one reply; the broker stamps identities.
pub struct ChannelClient<'s> {
    stream: &'s UnixStream,
    channel: ChannelId,
    /// The account handle every op names, on a credentialed channel.
    account: Option<String>,
}

impl<'s> ChannelClient<'s> {
    /// Bind to a credential-less channel on an established broker
    /// session.
    #[must_use]
    pub const fn new(stream: &'s UnixStream, channel: ChannelId) -> Self {
        Self {
            stream,
            channel,
            account: None,
        }
    }

    /// Bind to one account of a credentialed channel, by the handle
    /// [`accounts`](Self::accounts) listed.
    #[must_use]
    pub const fn with_account(stream: &'s UnixStream, channel: ChannelId, handle: String) -> Self {
        Self {
            stream,
            channel,
            account: Some(handle),
        }
    }

    /// The channel this client is bound to.
    #[must_use]
    pub const fn channel_id(&self) -> &ChannelId {
        &self.channel
    }

    /// The account handle this client acts as, if any.
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// The channel's accounts: broker-minted handles and their labels.
    /// Empty for a credential-less channel.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on transport failure, protocol violation, or
    /// denial (an unknown channel).
    pub fn accounts(&self) -> Result<Vec<ChannelAccount>, Error> {
        let op = ChannelOp::Accounts {
            channel: self.channel.0.clone(),
        };
        match self.request(&op)? {
            ChannelResult::Accounts { accounts } => Ok(accounts),
            ChannelResult::Denied => Err(Error::ChannelDenied {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::Message { .. }
            | ChannelResult::NoMessage
            | ChannelResult::Sent
            | ChannelResult::ChannelError
            | ChannelResult::UnknownAccount
            | ChannelResult::Redacted { .. } => Err(Error::ChannelProtocol {
                reason: String::from("broker gave another reply to an accounts op"),
            }),
        }
    }

    /// Receive the next inbound message, or `Ok(None)` if none is waiting.
    ///
    /// The returned [`Message`] carries broker-asserted sender and
    /// channel identity.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on transport failure, protocol violation, or
    /// channel backend failure (opaque).
    pub fn receive(&self) -> Result<Option<Message>, Error> {
        let op = ChannelOp::Receive {
            channel: self.channel.0.clone(),
            account: self.account.clone(),
        };
        match self.request(&op)? {
            ChannelResult::Message {
                sender,
                channel,
                text,
            } => Ok(Some(Message {
                sender: SenderId(sender),
                channel: ChannelId(channel),
                text,
            })),
            ChannelResult::NoMessage => Ok(None),
            ChannelResult::Sent
            | ChannelResult::Accounts { .. }
            | ChannelResult::Redacted { .. } => Err(Error::ChannelProtocol {
                reason: String::from("broker gave another reply to a receive op"),
            }),
            ChannelResult::UnknownAccount => Err(Error::ChannelUnknownAccount {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::Denied => Err(Error::ChannelDenied {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::ChannelError => Err(Error::ChannelBackend {
                channel: self.channel.0.clone(),
            }),
        }
    }

    /// Send a conversational reply. The broker lets it go without an
    /// approval only if it answers a message the broker delivered on
    /// this channel (once per message); otherwise it is denied.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on transport failure, protocol violation,
    /// denial, or channel backend failure (opaque).
    pub fn send_reply(&self, text: String) -> Result<(), Error> {
        self.send(text, false, None)
    }

    /// Send a state-changing message with a confirmation-rail
    /// approval: a token the broker minted for exactly this message
    /// . Any other string is denied, and presenting a token
    /// spends it.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on transport failure, protocol violation,
    /// denial, or channel backend failure (opaque).
    pub fn send_state_changing(&self, text: String, approval: String) -> Result<(), Error> {
        self.send(text, true, Some(approval))
    }

    fn send(
        &self,
        text: String,
        state_changing: bool,
        approval: Option<String>,
    ) -> Result<(), Error> {
        let op = ChannelOp::Send {
            channel: self.channel.0.clone(),
            text,
            state_changing,
            approval,
            account: self.account.clone(),
        };
        match self.request(&op)? {
            ChannelResult::Sent => Ok(()),
            ChannelResult::Message { .. }
            | ChannelResult::NoMessage
            | ChannelResult::Accounts { .. } => Err(Error::ChannelProtocol {
                reason: String::from("broker gave another reply to a send op"),
            }),
            ChannelResult::UnknownAccount => Err(Error::ChannelUnknownAccount {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::Denied => Err(Error::ChannelDenied {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::ChannelError => Err(Error::ChannelBackend {
                channel: self.channel.0.clone(),
            }),
            ChannelResult::Redacted { category, event_id } => Err(Error::ChannelRedacted {
                channel: self.channel.0.clone(),
                category,
                event_id,
            }),
        }
    }

    /// Sends one op and reads the broker's reply.
    ///
    /// The session speaks [`SessionRequest`]/[`SessionResponse`]
    /// envelopes (v4). Every op gets exactly one reply. EOF mid-op is a
    /// transport failure; a reply that is not a well-formed
    /// `SessionResponse` or that carries the wrong kind is a protocol
    /// violation. Fail-closed throughout.
    fn request(&self, op: &ChannelOp) -> Result<ChannelResult, Error> {
        let request = SessionRequest::Channel(op.clone());
        let payload = serde_json::to_vec(&request).map_err(|e| Error::ChannelProtocol {
            reason: format!("failed to encode channel op: {e}"),
        })?;
        write_frame(self.stream, &payload).map_err(frame_error)?;
        let reply = read_frame(self.stream).map_err(frame_error)?;
        let response: SessionResponse =
            serde_json::from_slice(&reply).map_err(|e| Error::ChannelProtocol {
                reason: format!("broker reply is not a valid session response: {e}"),
            })?;
        match response {
            SessionResponse::Channel(result) => Ok(result),
            SessionResponse::Vault(_) => Err(Error::ChannelProtocol {
                reason: String::from("broker gave a vault reply to a channel op"),
            }),
            SessionResponse::Egress(_) => Err(Error::ChannelProtocol {
                reason: String::from("broker gave an egress reply to a channel op"),
            }),
            SessionResponse::Memory(_) => Err(Error::ChannelProtocol {
                reason: String::from("broker gave a memory reply to a channel op"),
            }),
            SessionResponse::Skill(_) => Err(Error::ChannelProtocol {
                reason: String::from("broker gave a skill reply to a channel op"),
            }),
            SessionResponse::Goals(_)
            | SessionResponse::Query(_)
            | SessionResponse::Schedule(_)
            | SessionResponse::Hook(_)
            | SessionResponse::Artifact(_)
            | SessionResponse::Space(_)
            | SessionResponse::Subagent(_)
            | SessionResponse::Web(_)
            | SessionResponse::Browser(_) => Err(Error::ChannelProtocol {
                reason: String::from("broker gave a goals or query reply to a channel op"),
            }),
        }
    }
}

fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::ChannelIo { source };
    }
    Error::ChannelProtocol {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers one request with `reply`.
    fn answer_once(reply: SessionResponse) -> UnixStream {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
        }
        std::thread::spawn(move || {
            if read_frame(&broker).is_ok() {
                let _ = write_frame(&broker, &serde_json::to_vec(&reply).unwrap());
            }
        });
        agent
    }

    /// `request`, called directly: the channel's reply comes back, and a
    /// reply of another kind, a web reply among them, is a protocol
    /// violation.
    #[test]
    fn request_takes_a_channel_reply_and_refuses_another_kind() {
        let op = ChannelOp::Accounts {
            channel: String::from("local"),
        };
        let agent = answer_once(SessionResponse::Channel(ChannelResult::NoMessage));
        let client = ChannelClient::new(&agent, ChannelId::local());
        assert_eq!(client.request(&op).unwrap(), ChannelResult::NoMessage);
        for reply in [
            SessionResponse::Web(hatchling_core::WebResult::Off),
            SessionResponse::Skill(hatchling_core::SkillResult::SkillError),
        ] {
            let agent = answer_once(reply);
            let client = ChannelClient::new(&agent, ChannelId::local());
            assert!(matches!(
                client.request(&op),
                Err(Error::ChannelProtocol { .. })
            ));
        }
    }
}
