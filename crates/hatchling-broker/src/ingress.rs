//! The local channel's ingress: how a message reaches the
//! agent, and how its reply comes back.
//!
//! The broker binds a Unix socket at `HATCHLING_CHANNEL_INGRESS` and
//! owns it for its lifetime. The local user connects, writes one frame
//! per message (a JSON [`Message`](hatchling_channel::Message), in the
//! session's length-prefixed framing), and reads [`IngressEvent`]
//! frames back: `queued` or `refused` for each message, and `reply` for
//! each message the agent sends on the local channel. Every session's
//! local channel is backed by the ingress's [`LocalHub`].
//!
//! Who may connect is decided three ways, each on its own enough to keep
//! the cell out:
//!
//! - **Mode `0600`** ([`INGRESS_SOCKET_MODE`]), set at bind under the
//!   listen socket's stale-socket rule ([`bind_owner_socket`]). Never `0666`.
//! - **A kernel peer gate:** a connection is served only if its
//!   `SO_PEERCRED` uid is the socket's owner, the broker's own uid, and
//!   is closed before a byte is read otherwise.
//!   [`open_ingress`] refuses to start the ingress when that uid is the
//!   agent's, since the gate would then admit the cell.
//! - **Out of the cell's sight:** config parsing refuses an ingress
//!   path inside the broker socket's directory, the one host directory
//!   the cell mounts.
//!
//! The cell must not reach the ingress: a message it injected would be
//! "broker-delivered", and would bank the reply credit that lets a send
//! skip the rail.

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hatchling_channel::{Cleared, IngressEvent, LocalHub, Message, OutputGate};
use hatchling_core::framing::{read_frame_or_eof, write_frame};
use hatchling_core::{PeerUid, SocketPath, peer_credentials};

use crate::listener::bind_owner_socket;
use crate::serve::session_error;
use crate::{Config, Error};

/// The ingress socket's permission bits: owner only.
pub const INGRESS_SOCKET_MODE: u32 = 0o600;

/// How long a reply may block on a slow client before the send fails.
pub const INGRESS_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The bound ingress: its socket, its hub, and the one uid it serves.
#[derive(Debug)]
pub struct Ingress {
    listener: UnixListener,
    hub: LocalHub,
    peer_uid: PeerUid,
}

impl Ingress {
    /// Binds the ingress socket at `socket`, mode
    /// [`INGRESS_SOCKET_MODE`], and records its owner — the broker's own
    /// uid — as the only peer it will serve. Every reply its hub delivers
    /// passes `gate`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SocketPathNotSocket`] or [`Error::SocketSetup`]
    /// as [`bind_owner_socket`] does.
    pub fn bind(socket: &SocketPath, gate: OutputGate) -> Result<Self, Error> {
        // Made for the broker's owner, recorded at start, never read back
        // from a path that could be swapped.
        let owner = crate::owner::owner()?;
        let listener = bind_owner_socket(socket, INGRESS_SOCKET_MODE, owner)?;
        Ok(Self {
            listener,
            hub: LocalHub::new(gate),
            peer_uid: PeerUid::from_kernel(owner),
        })
    }

    /// The hub every session's local channel is backed by.
    #[must_use]
    pub const fn hub(&self) -> &LocalHub {
        &self.hub
    }

    /// The only uid this ingress serves.
    #[must_use]
    pub const fn peer_uid(&self) -> PeerUid {
        self.peer_uid
    }

    /// Serves clients one at a time, forever. A failed connection is
    /// logged and the next one accepted.
    pub fn serve(&self) -> ! {
        loop {
            if let Err(error) = self.serve_one() {
                tracing::warn!(%error, "channel ingress connection failed");
            }
        }
    }

    /// Accepts one client and serves it until it disconnects.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Accept`] if accepting fails,
    /// [`Error::PeerUnauthorized`] for a peer other than the owner
    /// (closed before a byte is read), and [`Error::SessionIo`] or
    /// [`Error::SessionProtocol`] when the client's stream fails or
    /// carries something other than a message.
    pub fn serve_one(&self) -> Result<(), Error> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|source| Error::Accept { source })?;
        self.serve_client(&stream)
    }

    fn serve_client(&self, stream: &UnixStream) -> Result<(), Error> {
        // The peer gate: kernel credentials before bytes.
        let creds = peer_credentials(stream)?;
        if creds.uid() != self.peer_uid {
            return Err(Error::PeerUnauthorized {
                actual: creds.uid(),
                expected: self.peer_uid,
            });
        }
        stream
            .set_write_timeout(Some(INGRESS_WRITE_TIMEOUT))
            .map_err(|source| Error::SessionIo { source })?;
        let writer = Arc::new(Mutex::new(
            stream
                .try_clone()
                .map_err(|source| Error::SessionIo { source })?,
        ));
        let sink_writer = Arc::clone(&writer);
        let sink = self.hub.attach(Box::new(move |cleared: &Cleared| {
            send_event(
                &sink_writer,
                &IngressEvent::Reply {
                    text: cleared.message().text.clone(),
                },
            )
            .map_err(|error| hatchling_channel::Error::Backend(error.to_string()))
        }));
        let served = self.read_messages(stream, &writer);
        self.hub.detach(sink);
        served
    }

    /// Queues each message the client sends, answering each with the
    /// hub's verdict, until the client closes.
    fn read_messages(&self, stream: &UnixStream, writer: &Mutex<UnixStream>) -> Result<(), Error> {
        while let Some(frame) = read_frame_or_eof(stream).map_err(session_error)? {
            let msg: Message =
                serde_json::from_slice(&frame).map_err(|e| Error::SessionProtocol {
                    reason: format!("ingress frame is not a channel message: {e}"),
                })?;
            send_event(writer, &self.hub.inject(msg))?;
        }
        Ok(())
    }
}

/// Writes one event frame to the client.
fn send_event(writer: &Mutex<UnixStream>, event: &IngressEvent) -> Result<(), Error> {
    let payload = serde_json::to_vec(event).map_err(|e| Error::SessionProtocol {
        reason: format!("ingress event encode failed: {e}"),
    })?;
    let stream = writer.lock().unwrap_or_else(PoisonError::into_inner);
    write_frame(&stream, &payload).map_err(session_error)
}

/// The broker's ingress, bound, when one is configured, its replies
/// passing `gate`.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when the ingress would serve the
/// agent's own uid (the socket is unlinked again first), or the bind
/// errors of [`Ingress::bind`].
pub fn open_ingress(config: &Config, gate: OutputGate) -> Result<Option<Ingress>, Error> {
    let Some(socket) = &config.channel_ingress else {
        return Ok(None);
    };
    let ingress = Ingress::bind(socket, gate)?;
    if ingress.peer_uid() == config.agent_uid {
        let _ = std::fs::remove_file(socket.as_path());
        return Err(Error::InvalidConfig {
            key: "HATCHLING_CHANNEL_INGRESS",
            reason: format!(
                "the ingress serves uid {}, the broker's own, which is also the agent's: \
                 the cell could inject its own messages; {}",
                config.agent_uid,
                crate::cell::OWN_UID_HINT
            ),
        });
    }
    Ok(Some(ingress))
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_channel::{ChannelId, MAX_INGRESS_TEXT_BYTES, SenderId};
    use hatchling_core::framing::read_frame;

    use super::*;
    use crate::VaultConfig;
    use crate::test_socket::TestSocket;

    /// Removes `socket` when the test ends, pass or fail.
    fn guard(socket: &SocketPath) -> TestSocket {
        TestSocket::new(socket.as_path().to_string_lossy())
    }

    fn real_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// A socket path under `/tmp` (`sun_path` is short; `TMPDIR` may be
    /// deep), unique per test and process.
    fn path(name: &str) -> SocketPath {
        format!("/tmp/hlin-{}-{name}.sock", std::process::id())
            .parse()
            .unwrap()
    }

    fn bound(name: &str) -> (Ingress, SocketPath) {
        let socket = path(name);
        let _ = std::fs::remove_file(socket.as_path());
        (
            Ingress::bind(&socket, OutputGate::disabled()).unwrap(),
            socket,
        )
    }

    fn client(socket: &SocketPath) -> UnixStream {
        let stream = UnixStream::connect(socket.as_path()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn msg(text: &str) -> Message {
        Message {
            sender: SenderId(String::from("claimed-sender")),
            channel: ChannelId(String::from("claimed-channel")),
            text: text.to_owned(),
        }
    }

    fn inject(stream: &UnixStream, msg: &Message) -> IngressEvent {
        write_frame(stream, &serde_json::to_vec(msg).unwrap()).unwrap();
        event(stream)
    }

    fn event(stream: &UnixStream) -> IngressEvent {
        serde_json::from_slice(&read_frame(stream).unwrap()).unwrap()
    }

    fn config(agent_uid: u32, ingress: Option<SocketPath>) -> Config {
        let root = std::path::PathBuf::from("/nonexistent/hatchling-ingress-vault");
        Config {
            socket: "/tmp/hatchling-test-ingress-dummy/broker.sock"
                .parse()
                .unwrap(),
            agent_uid: PeerUid::from_kernel(agent_uid),
            socket_mode: 0o600,
            max_connections: crate::DEFAULT_MAX_CONNECTIONS,
            scheduler: crate::SchedulerConfig::disabled(),
            log_level: hatchling_core::LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&root),
                root,
                grants: Vec::new(),
            },
            egress: crate::EgressConfig {
                allowlist: crate::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: vec![String::from("local")],
            channel_accounts: Vec::new(),
            channel_ingress: ingress,
            skills: crate::SkillConfig::disabled(),
            memory: crate::MemoryConfig {
                root: std::path::PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: crate::ArtifactConfig::disabled(),
            spaces: crate::SpaceConfig::disabled(),
            web: crate::WebConfig::off(),
            goals: crate::GoalsConfig::disabled(),
        }
    }

    #[test]
    fn the_ingress_socket_is_owner_only_and_serves_its_owner() {
        let (ingress, socket) = bound("mode");
        let _socket = guard(&socket);
        let meta = std::fs::metadata(socket.as_path()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(ingress.peer_uid(), PeerUid::from_kernel(real_uid()));
        assert_eq!(INGRESS_SOCKET_MODE, 0o600);
        // A stale ingress socket from an earlier run is replaced.
        drop(ingress);
        let (again, _) = bound("mode");
        assert_eq!(again.peer_uid(), PeerUid::from_kernel(real_uid()));
    }

    #[test]
    fn a_non_socket_at_the_ingress_path_fails_closed() {
        let socket = path("file");
        let _socket = guard(&socket);
        std::fs::write(socket.as_path(), b"not a socket").unwrap();
        assert!(matches!(
            Ingress::bind(&socket, OutputGate::disabled()),
            Err(Error::SocketPathNotSocket { .. })
        ));
        assert_eq!(std::fs::read(socket.as_path()).unwrap(), b"not a socket");
    }

    #[test]
    fn a_client_injects_and_reads_replies_until_it_disconnects() {
        let (ingress, socket) = bound("client");
        let _socket = guard(&socket);
        std::thread::scope(|scope| {
            let served = scope.spawn(|| ingress.serve_one());
            let stream = client(&socket);
            assert_eq!(inject(&stream, &msg("hello")), IngressEvent::Queued);
            // Queued means queued: the hub holds it, claims and all (the
            // broker stamps identity on receive, not here).
            let queued = ingress.hub().take().unwrap();
            assert_eq!(queued, msg("hello"));
            // A reply the agent sends reaches this client.
            ingress.hub().deliver(&msg("hi back"), "s").unwrap();
            assert_eq!(
                event(&stream),
                IngressEvent::Reply {
                    text: String::from("hi back")
                }
            );
            // Over the text bound: refused, not queued.
            let long = "x".repeat(MAX_INGRESS_TEXT_BYTES + 1);
            assert_eq!(inject(&stream, &msg(&long)), IngressEvent::Refused);
            assert_eq!(ingress.hub().queued(), 0);
            drop(stream);
            served.join().unwrap().unwrap();
        });
        // The client is gone, and so is its reply path.
        assert!(ingress.hub().deliver(&msg("late"), "s").is_err());
    }

    #[test]
    fn a_frame_that_is_not_a_message_ends_the_client() {
        let (ingress, socket) = bound("garbage");
        let _socket = guard(&socket);
        std::thread::scope(|scope| {
            let served = scope.spawn(|| ingress.serve_one());
            let stream = client(&socket);
            write_frame(&stream, br#"{"text":7}"#).unwrap();
            let result = served.join().unwrap();
            assert!(
                matches!(&result, Err(Error::SessionProtocol { reason }) if reason.contains("not a channel message")),
                "{result:?}"
            );
            assert!(
                matches!(read_frame_or_eof(&stream), Ok(None)),
                "closed, no reply"
            );
        });
        assert_eq!(ingress.hub().queued(), 0);
        assert!(
            ingress.hub().deliver(&msg("late"), "s").is_err(),
            "detached"
        );
    }

    #[test]
    fn a_peer_that_is_not_the_owner_never_reaches_the_hub() {
        let (mut ingress, socket) = bound("gate");
        let _socket = guard(&socket);
        // Stand in for another uid: this process is now "not the owner".
        ingress.peer_uid = PeerUid::from_kernel(real_uid().wrapping_add(1));
        std::thread::scope(|scope| {
            let served = scope.spawn(|| ingress.serve_one());
            let stream = client(&socket);
            let _ = write_frame(&stream, &serde_json::to_vec(&msg("sneak")).unwrap());
            let result = served.join().unwrap();
            assert!(
                matches!(result, Err(Error::PeerUnauthorized { .. })),
                "{result:?}"
            );
            match read_frame_or_eof(&stream) {
                Ok(None) => {}
                Err(hatchling_core::Error::FrameIo { source })
                    if source.kind() == ErrorKind::ConnectionReset => {}
                other => assert!(
                    other.is_err(),
                    "no reply to an unauthorized peer: {other:?}"
                ),
            }
        });
        assert_eq!(ingress.hub().queued(), 0, "nothing was queued");
        assert!(
            ingress.hub().deliver(&msg("x"), "s").is_err(),
            "no sink attached"
        );
    }

    #[test]
    fn the_ingress_is_refused_when_it_would_serve_the_agent() {
        let off = OutputGate::disabled;
        assert!(
            open_ingress(&config(real_uid(), None), off())
                .unwrap()
                .is_none()
        );

        let socket = path("open-refused");
        let _refused = guard(&socket);
        let err = open_ingress(&config(real_uid(), Some(socket.clone())), off()).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidConfig { key: "HATCHLING_CHANNEL_INGRESS", reason }
                if reason.contains("also the agent's") && reason.contains("/etc/subuid")),
            "{err:?}"
        );
        assert!(!socket.as_path().exists(), "the refused socket is unlinked");

        let socket = path("open-ok");
        let _socket = guard(&socket);
        let ingress = open_ingress(&config(real_uid().wrapping_add(1), Some(socket)), off())
            .unwrap()
            .unwrap();
        assert_eq!(ingress.peer_uid(), PeerUid::from_kernel(real_uid()));
    }
}
