//! The egress authority (WO-017, step 2): the process that alone holds the
//! credentials the broker's outgoing requests use, and only while it puts
//! one into a request it makes itself.
//!
//! [`start`] runs in the broker's start, right after the vault keeper's,
//! while the broker still has one thread. It starts the broker's own binary
//! again as `egress-authority`, joined to the broker by a socket pair of
//! its own and to the keeper by the end of a pair the broker made when it
//! started the keeper. The broker keeps neither end of the keeper's pair,
//! so what reaches the keeper on it comes from the authority alone: the
//! keeper releases the authority's purposes there, and refuses them on the
//! broker's own channel.
//!
//! **Connections.** The pair with the broker carries only connections:
//! for each, the broker makes a socket pair and sends the authority one
//! end ([`Authority::connect`]), which it serves on a thread of its own.
//! A session's egress is one connection for the session's life; a search
//! is one connection for one search. So nothing waits on another's
//! request, and a connection's state (a session's surrogate key) is gone
//! when its end closes.
//!
//! **What it does now:**
//! - **The egress swap** (slice 2). A session's connection opens its
//!   egress ([`Connection::egress`]): the authority builds the session a
//!   proxy of its own, with a fresh surrogate key, over the allowlist it
//!   resolved once. It mints the cell's surrogates and forwards its
//!   requests, putting in each destination's credential, which it reads
//!   from the keeper for that request alone. The broker checks the
//!   session's grants first and relays the requests and answers
//!   ([`EgressLink`]); it never holds a credential or the surrogate key.
//!   The authority checks a destination against the operator's grants
//!   too, so a broker can't send it anywhere no agent may go.
//! - **Web search** (slice 4). The broker sends a query; the authority
//!   reads the search provider's key from the keeper, runs the search,
//!   and answers with the results.
//! - **The harness relay** (slice 3). A harness run's connection names
//!   its harness and the surrogate its cell holds ([`Connection::relay`]).
//!   The authority finds the harness's vendor in its own configuration
//!   (its API, its one destination, its pinned addresses) and reads the
//!   vendor key from the keeper, so a missing key refuses the run before
//!   its cell starts. When the cell is about to start, the broker hands
//!   over the run's handoff socket ([`PendingRelay::start`]): the
//!   authority takes the cell's listener from it and serves the relay
//!   ([`crate::harness_relay`]), swapping the surrogate for the key, until
//!   the broker asks how it went ([`AuthorityRelay::finish`]) or the
//!   connection closes. The broker never holds the key, or names where it
//!   goes.
//!
//! **What it holds against.** A compromised broker can't read a key the
//! authority holds: the keeper releases it to the authority's channel
//! alone, and the authority isn't dumpable. It is not yet an
//! operating-system boundary: the broker starts it as its own user, and a
//! root broker can reach any process. And until the spawner split, the
//! peer uid a request is bound to is the broker's word. The spawner split
//! gives each its own user.

use std::collections::BTreeMap;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::{SecretName, fits_one_frame};
use hatchling_egress::search::{Hit, SearchError};
use hatchling_egress::web::WebUrl;
use hatchling_egress::{
    EgressRequest, EgressResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Proxy, TlsClient,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::egress_path::{EgressPath, Refusal, RefusalKind};
use crate::harness::HarnessKind;
use crate::harness_relay::{Relay, RelayCount, Vendor};
use crate::keeper::Keeper;
use crate::serve::{destination_to_secret_name, egress_tls};
use crate::web_ops::{FETCH_DEADLINE, SEARCH_KEY};
use crate::{Config, Error};

/// The authority's subcommand: `hatchling-broker egress-authority`,
/// started only by the broker.
pub const EGRESS_AUTHORITY_SUBCOMMAND: &str = "egress-authority";

/// The authority's user when the broker runs as root (WO-017, step 3).
pub const EGRESS_USER: &str = "hatchling-egress";

/// How long the broker waits for the authority to say it started.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// The setting that names the authority's end of its pair with the broker.
const BROKER_FD: &str = crate::environment::HATCHLING_AUTHORITY_FD.name;

/// The setting that names the authority's end of its pair with the keeper.
const KEEPER_FD: &str = crate::environment::HATCHLING_AUTHORITY_KEEPER_FD.name;

/// How long the broker waits for each of a connection's frames: past the
/// longest the authority itself waits on an upstream read (a local model
/// may think for minutes).
const ANSWER_TIMEOUT: Duration =
    Duration::from_secs(hatchling_egress::local::IO_TIMEOUT.as_secs() + 30);

/// The most bytes of a body one frame carries: a body moves as many.
const PART_BYTES: usize = 60 * 1024;

/// The longest a harness run's relay serves: past any run's timeout and
/// its grace.
const MAX_RELAY: Duration = Duration::from_secs(2 * 3600);

/// What the broker is told when the authority can't be reached, or
/// answers out of step.
const UNAVAILABLE: &str = "the egress authority is unavailable";

/// Runs one search with the provider's key.
pub type Provider = Arc<dyn Fn(&[u8], &str) -> Result<Vec<Hit>, SearchError> + Send + Sync>;

/// What the broker asks on a connection.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    /// A web search for `query`.
    Search { query: String },
    /// Open this connection's egress: a proxy of its own.
    Egress,
    /// Mint a surrogate for `peer_uid` to reach `destination`.
    Mint {
        peer_uid: u32,
        destination: String,
        ttl_secs: u64,
    },
    /// Forward a request presented by `peer_uid`. Its body follows in
    /// `parts` frames of its own.
    Forward {
        peer_uid: u32,
        method: String,
        destination: String,
        path: String,
        headers: Vec<(String, String)>,
        surrogate_b64: String,
        parts: usize,
    },
    /// Make ready a harness run's relay: `harness` names it, and
    /// `surrogate` is what its cell holds.
    Relay { harness: String, surrogate: String },
    /// Start the relay made ready, serving for `seconds`. The run's
    /// handoff socket follows, as a descriptor.
    Start { seconds: u64 },
    /// Stop the relay, and say what it did.
    Finish,
}

/// What the authority answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    /// A search's results, the provider's words.
    Results { hits: Vec<Found> },
    /// A search refused, and why: the broker's to tell the agent.
    Refused { reason: String },
    /// The connection's egress is open.
    Ready,
    /// The connection's egress couldn't be opened, and why.
    Failed { reason: String },
    /// A minted surrogate.
    Surrogate { surrogate_b64: String },
    /// A forwarded request's answer. Its body follows in `parts` frames
    /// of its own.
    Answer {
        status: u16,
        headers: Vec<(String, String)>,
        parts: usize,
    },
    /// A mint or a request refused, or failed.
    Denied { refusal: Refusal },
    /// What a relay did, and the trouble it had, if any.
    Relayed {
        forwarded: u32,
        refused: u32,
        problem: Option<String>,
    },
}

/// One search result, as it crosses to the broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Found {
    title: String,
    url: String,
    snippet: String,
}

/// The broker's end of the authority.
#[derive(Debug)]
pub struct Authority {
    /// Carries the connections' ends, one at a time.
    control: Mutex<UnixStream>,
    /// Set once a connection couldn't be handed over: none is again.
    lost: AtomicBool,
    /// Kept so the authority isn't left a zombie; it leaves when the
    /// broker does.
    _child: Option<Child>,
}

/// The broker's authority, once started.
static AUTHORITY: OnceLock<Authority> = OnceLock::new();

/// The authority this broker started, if it started one.
pub(crate) fn authority() -> Option<&'static Authority> {
    AUTHORITY.get()
}

/// Starts the authority, given `keeper`: its end of the pair the broker
/// made with the keeper.
///
/// # Errors
///
/// As [`Authority::start_with`], and [`Error::Authority`] when the broker's
/// binary can't be found or an authority is already running.
pub fn start(keeper: UnixStream) -> Result<(), Error> {
    let program = std::env::current_exe().map_err(|e| Error::Authority {
        reason: format!("cannot locate the broker binary: {e}"),
    })?;
    let authority = Authority::start_with(Command::new(program), &keeper)?;
    drop(keeper);
    AUTHORITY.set(authority).map_err(|_| Error::Authority {
        reason: String::from("an egress authority is already running"),
    })
}

impl Authority {
    /// Starts the authority as `program`, the broker's binary with its
    /// environment, given its subcommand, its end of a new pair with the
    /// broker, and `keeper`, its end of the pair with the keeper; as root,
    /// as [`EGRESS_USER`]. Returns once it says it started.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when it can't be started, refuses to
    /// start (its reason, as it gave it), or doesn't say within
    /// [`START_TIMEOUT`].
    pub fn start_with(mut program: Command, keeper: &UnixStream) -> Result<Self, Error> {
        let failed = |reason: String| Error::Authority { reason };
        if let Some((uid, gid)) = egress_user()? {
            program.uid(uid).gid(gid);
        }
        let (ours, theirs) =
            UnixStream::pair().map_err(|e| failed(format!("cannot make the socket pair: {e}")))?;
        for end in [theirs.as_fd(), keeper.as_fd()] {
            hatchling_sys::keep_across_exec(end)
                .map_err(|e| failed(format!("cannot hand the authority its socket: {e}")))?;
        }
        let child = program
            .arg(EGRESS_AUTHORITY_SUBCOMMAND)
            .env(BROKER_FD, theirs.as_raw_fd().to_string())
            .env(KEEPER_FD, keeper.as_raw_fd().to_string())
            .spawn()
            .map_err(|e| failed(format!("cannot start: {e}")))?;
        drop(theirs);
        let authority = Self {
            _child: Some(child),
            ..Self::over(ours)
        };
        authority.started()?;
        Ok(authority)
    }

    /// Waits for the authority to say it started: an empty frame, or why
    /// it won't.
    ///
    /// # Errors
    ///
    /// [`Error::Authority`] with its reason, or when it says nothing.
    fn started(&self) -> Result<(), Error> {
        let failed = |reason: String| Error::Authority { reason };
        let control = self
            .control
            .lock()
            .map_err(|_| failed(String::from("its channel was poisoned")))?;
        control
            .set_read_timeout(Some(START_TIMEOUT))
            .map_err(|e| failed(format!("cannot bound its start: {e}")))?;
        let said = read_frame(&control).map_err(|e| failed(format!("didn't start: {e}")))?;
        control
            .set_read_timeout(None)
            .map_err(|e| failed(format!("cannot wait on it: {e}")))?;
        drop(control);
        if said.is_empty() {
            Ok(())
        } else {
            Err(failed(String::from_utf8_lossy(&said).into_owned()))
        }
    }

    /// The broker's end, `control`, of an authority already running.
    #[must_use]
    pub const fn over(control: UnixStream) -> Self {
        Self {
            control: Mutex::new(control),
            lost: AtomicBool::new(false),
            _child: None,
        }
    }

    /// A new connection to the authority, served on a thread of its own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when it can't be made or handed over.
    pub fn connect(&self) -> Result<Connection, Error> {
        let (ours, theirs) = UnixStream::pair().map_err(gone)?;
        let control = self.control.lock().map_err(|_| Error::Authority {
            reason: String::from("an earlier connection to it panicked"),
        })?;
        if self.lost.load(Ordering::SeqCst) {
            return Err(Error::Authority {
                reason: String::from("lost: a connection couldn't be handed over"),
            });
        }
        let sent = hatchling_sys::net::send_fd(control.as_fd(), theirs.as_fd());
        drop(control);
        if let Err(error) = sent {
            self.lost.store(true, Ordering::SeqCst);
            return Err(gone(error));
        }
        drop(theirs);
        Connection::over(ours)
    }

    /// Searches the web for `query`, through the authority: the results,
    /// or why there are none.
    ///
    /// # Errors
    ///
    /// A sentence for the agent: the authority refused, or can't be
    /// reached.
    pub fn search(&self, query: &str) -> Result<Vec<Hit>, String> {
        self.connect()
            .map_err(|error| {
                tracing::error!(%error, "the egress authority can't be reached");
                String::from(UNAVAILABLE)
            })?
            .search(query)
    }
}

/// One connection to the authority: the broker's end.
#[derive(Debug)]
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    /// The broker's end, `stream`, of a connection the authority serves.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when its answers can't be bounded.
    pub fn over(stream: UnixStream) -> Result<Self, Error> {
        stream
            .set_read_timeout(Some(ANSWER_TIMEOUT))
            .map_err(|e| Error::Authority {
                reason: format!("cannot bound its answers: {e}"),
            })?;
        Ok(Self { stream })
    }

    /// Searches the web for `query`.
    ///
    /// # Errors
    ///
    /// A sentence for the agent: the authority refused, or can't be
    /// reached.
    pub fn search(&self, query: &str) -> Result<Vec<Hit>, String> {
        let request = Request::Search {
            query: query.to_owned(),
        };
        let answer = send(&self.stream, &request).and_then(|()| receive(&self.stream));
        let hits = match answer {
            Ok(Reply::Results { hits }) => hits,
            Ok(Reply::Refused { reason }) => return Err(reason),
            Ok(
                Reply::Ready
                | Reply::Failed { .. }
                | Reply::Surrogate { .. }
                | Reply::Answer { .. }
                | Reply::Denied { .. }
                | Reply::Relayed { .. },
            ) => return Err(String::from(UNAVAILABLE)),
            Err(error) => {
                tracing::error!(%error, "the egress authority can't be reached");
                return Err(String::from(UNAVAILABLE));
            }
        };
        hits.into_iter()
            .map(|found| {
                WebUrl::parse(&found.url)
                    .map(|url| Hit {
                        title: found.title,
                        url,
                        snippet: found.snippet,
                    })
                    .map_err(|_| String::from("the egress authority answered a bad result"))
            })
            .collect()
    }

    /// Opens this connection's egress: a session's, for its life.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when the authority couldn't open it
    /// (its allowlist doesn't resolve) or can't be reached.
    pub fn egress(self) -> Result<EgressLink, Error> {
        send(&self.stream, &Request::Egress)?;
        match receive(&self.stream)? {
            Reply::Ready => Ok(EgressLink {
                stream: Mutex::new(self.stream),
                lost: AtomicBool::new(false),
            }),
            Reply::Failed { reason } => Err(Error::Authority { reason }),
            Reply::Results { .. }
            | Reply::Refused { .. }
            | Reply::Surrogate { .. }
            | Reply::Answer { .. }
            | Reply::Denied { .. }
            | Reply::Relayed { .. } => Err(out_of_step()),
        }
    }
}

impl Connection {
    /// Makes ready a harness run's relay: the authority reads `harness`'s
    /// vendor key now, so a run with none is refused before its cell
    /// starts. `surrogate` is what the cell holds.
    ///
    /// # Errors
    ///
    /// The outer error: the authority can't be reached, or answered out
    /// of step. The inner one: the run is refused, in words for its
    /// report.
    pub fn relay(
        self,
        harness: HarnessKind,
        surrogate: &str,
    ) -> Result<Result<PendingRelay, String>, Error> {
        let request = Request::Relay {
            harness: harness.name().to_owned(),
            surrogate: surrogate.to_owned(),
        };
        send(&self.stream, &request)?;
        match receive(&self.stream)? {
            Reply::Ready => Ok(Ok(PendingRelay { connection: self })),
            Reply::Refused { reason } => Ok(Err(reason)),
            Reply::Failed { reason } => Err(Error::Authority { reason }),
            Reply::Results { .. }
            | Reply::Surrogate { .. }
            | Reply::Answer { .. }
            | Reply::Denied { .. }
            | Reply::Relayed { .. } => Err(out_of_step()),
        }
    }
}

/// A harness run's relay, made ready: the vendor key read.
#[derive(Debug)]
pub struct PendingRelay {
    connection: Connection,
}

impl PendingRelay {
    /// Starts the relay: the authority takes the cell's listener from
    /// `handoff` and serves it for `serving`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when the authority can't be reached,
    /// or couldn't start it.
    pub fn start(self, handoff: &UnixListener, serving: Duration) -> Result<AuthorityRelay, Error> {
        let stream = &self.connection.stream;
        send(
            stream,
            &Request::Start {
                seconds: serving.as_secs(),
            },
        )?;
        hatchling_sys::net::send_fd(stream.as_fd(), handoff.as_fd()).map_err(gone)?;
        match receive(stream)? {
            Reply::Ready => Ok(AuthorityRelay {
                connection: self.connection,
            }),
            Reply::Failed { reason } => Err(Error::Authority { reason }),
            Reply::Results { .. }
            | Reply::Refused { .. }
            | Reply::Surrogate { .. }
            | Reply::Answer { .. }
            | Reply::Denied { .. }
            | Reply::Relayed { .. } => Err(out_of_step()),
        }
    }
}

/// A harness run's relay, serving in the authority.
#[derive(Debug)]
pub struct AuthorityRelay {
    connection: Connection,
}

impl AuthorityRelay {
    /// Stops the relay and reports what it did, and the trouble it had.
    #[must_use]
    pub fn finish(self) -> (RelayCount, Option<String>) {
        let stream = &self.connection.stream;
        let answer = send(stream, &Request::Finish).and_then(|()| receive(stream));
        match answer {
            Ok(Reply::Relayed {
                forwarded,
                refused,
                problem,
            }) => (RelayCount { forwarded, refused }, problem),
            Ok(Reply::Failed { reason }) => (RelayCount::default(), Some(reason)),
            Ok(
                Reply::Results { .. }
                | Reply::Refused { .. }
                | Reply::Ready
                | Reply::Surrogate { .. }
                | Reply::Answer { .. }
                | Reply::Denied { .. },
            ) => (RelayCount::default(), Some(out_of_step().to_string())),
            Err(error) => (RelayCount::default(), Some(error.to_string())),
        }
    }
}

fn out_of_step() -> Error {
    Error::Authority {
        reason: String::from("it answered out of step"),
    }
}

/// A session's egress, through the authority: the broker's end.
#[derive(Debug)]
pub struct EgressLink {
    stream: Mutex<UnixStream>,
    /// Set once an exchange broke off: a late answer could be read as the
    /// next one's, so no request is sent again.
    lost: AtomicBool,
}

impl EgressLink {
    /// Runs one exchange alone. One that breaks off, or is answered out
    /// of step, loses the link.
    fn exchange<T>(
        &self,
        run: impl FnOnce(&UnixStream) -> Result<Option<T>, Error>,
    ) -> Result<T, Refusal> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| Refusal::failed(UNAVAILABLE))?;
        if self.lost.load(Ordering::SeqCst) {
            return Err(Refusal::failed(UNAVAILABLE));
        }
        let outcome = run(&stream);
        drop(stream);
        match outcome {
            Ok(Some(answer)) => Ok(answer),
            Ok(None) => {
                self.lost.store(true, Ordering::SeqCst);
                tracing::error!("the egress authority answered out of step");
                Err(Refusal::failed(UNAVAILABLE))
            }
            Err(error) => {
                self.lost.store(true, Ordering::SeqCst);
                tracing::error!(%error, "the egress authority can't be reached");
                Err(Refusal::failed(UNAVAILABLE))
            }
        }
    }
}

impl EgressPath for EgressLink {
    fn mint(&self, peer_uid: u32, destination: &str, ttl_secs: u64) -> Result<String, Refusal> {
        let request = Request::Mint {
            peer_uid,
            destination: destination.to_owned(),
            ttl_secs,
        };
        self.exchange(|stream| {
            send(stream, &request)?;
            Ok(match receive(stream)? {
                Reply::Surrogate { surrogate_b64 } => Some(Ok(surrogate_b64)),
                Reply::Denied { refusal } => Some(Err(refusal)),
                Reply::Results { .. }
                | Reply::Refused { .. }
                | Reply::Ready
                | Reply::Failed { .. }
                | Reply::Answer { .. }
                | Reply::Relayed { .. } => None,
            })
        })?
    }

    fn forward(&self, peer_uid: u32, request: &EgressRequest) -> Result<EgressResponse, Refusal> {
        let header = Request::Forward {
            peer_uid,
            method: request.method.clone(),
            destination: request.destination.clone(),
            path: request.path.clone(),
            headers: request.headers.clone(),
            surrogate_b64: request.surrogate_b64.clone(),
            parts: request.body.chunks(PART_BYTES).count(),
        };
        self.exchange(|stream| {
            send(stream, &header)?;
            for part in request.body.chunks(PART_BYTES) {
                write_frame(stream, part).map_err(gone)?;
            }
            Ok(match receive(stream)? {
                Reply::Answer {
                    status,
                    headers,
                    parts,
                } => Some(Ok(EgressResponse {
                    status,
                    headers,
                    body: read_parts(stream, parts, MAX_RESPONSE_BYTES)?,
                })),
                Reply::Denied { refusal } => Some(Err(refusal)),
                Reply::Results { .. }
                | Reply::Refused { .. }
                | Reply::Ready
                | Reply::Failed { .. }
                | Reply::Surrogate { .. }
                | Reply::Relayed { .. } => None,
            })
        })?
    }
}

/// Writes `message` as one frame.
fn send(stream: &UnixStream, message: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(message).map_err(|e| Error::Authority {
        reason: format!("cannot write a message: {e}"),
    })?;
    write_frame(stream, &bytes).map_err(gone)
}

/// Reads one reply.
fn receive(stream: &UnixStream) -> Result<Reply, Error> {
    let frame = read_frame(stream).map_err(gone)?;
    serde_json::from_slice(&frame).map_err(|e| Error::Authority {
        reason: format!("an answer that doesn't parse: {e}"),
    })
}

/// Reads a body sent as `parts` frames, refusing one that would be more
/// than `max` bytes.
fn read_parts(stream: &UnixStream, parts: usize, max: usize) -> Result<Vec<u8>, Error> {
    let too_large = || Error::Authority {
        reason: format!("a body of more than {max} bytes"),
    };
    if parts > max.div_ceil(PART_BYTES) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    for _ in 0..parts {
        let part = read_frame(stream).map_err(gone)?;
        if part.len() > PART_BYTES || body.len() + part.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&part);
    }
    Ok(body)
}

fn gone(error: impl std::fmt::Display) -> Error {
    Error::Authority {
        reason: format!("cannot be reached: {error}"),
    }
}

/// The authority: `hatchling-broker egress-authority`.
#[must_use]
pub fn egress_authority_main() -> ExitCode {
    let inherited = |fd: Result<String, std::env::VarError>| {
        fd.ok()
            .and_then(|fd| fd.parse::<i32>().ok())
            .map(hatchling_sys::net::inherited_unix_stream)
    };
    let (broker, keeper) = match (
        inherited(std::env::var(BROKER_FD)),
        inherited(std::env::var(KEEPER_FD)),
    ) {
        (Some(Ok(broker)), Some(Ok(keeper))) => (broker, keeper),
        (Some(Err(error)), _) | (_, Some(Err(error))) => return refuse_to_run(&error.to_string()),
        (None, _) | (_, None) => return refuse_to_run("only the broker starts it"),
    };
    let begun = hatchling_sys::set_not_dumpable()
        .map_err(|source| Error::Authority {
            reason: format!("cannot keep its memory to itself: {source}"),
        })
        .and_then(|()| Config::from_env())
        .and_then(|config| {
            let _ = tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_max_level(config.log_level.to_tracing())
                .try_init();
            // Which user it runs as: as root, its own.
            tracing::info!(euid = hatchling_sys::euid(), "the egress authority started");
            let tls = egress_tls(|key| std::env::var(key).ok())?;
            let provider = config.web.search.then(|| {
                let tls = tls.clone();
                let provider: Provider =
                    Arc::new(move |key, query| hatchling_egress::search::brave(&tls, key, query));
                provider
            });
            let vendors = crate::harness::Harnesses::from_env(&config)?
                .with_tls(&tls)
                .vendors()
                .clone();
            Keeper::over(keeper).map(|keeper| Shared {
                config,
                keeper,
                tls,
                provider,
                deadline: FETCH_DEADLINE,
                vendors,
            })
        });
    match begun {
        // The broker waits for its word: started, or why not.
        Ok(shared) => {
            if let Err(error) = write_frame(&broker, b"") {
                return refuse_to_run(&format!("cannot tell the broker it started: {error}"));
            }
            serve(&broker, &Arc::new(shared));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = write_frame(&broker, error.to_string().as_bytes());
            refuse_to_run(&error.to_string())
        }
    }
}

/// The authority's user, its uid and gid, when this process runs as root;
/// none otherwise.
///
/// # Errors
///
/// [`Error::Authority`] when this process runs as root and there's no
/// such user, or it is root itself.
pub fn egress_user() -> Result<Option<(u32, u32)>, Error> {
    crate::keeper::service_user(EGRESS_USER, "its egress authority")
        .map_err(|reason| Error::Authority { reason })
}

fn refuse_to_run(reason: &str) -> ExitCode {
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "hatchling-broker egress-authority: {reason}"
    );
    ExitCode::FAILURE
}

/// What every connection the authority serves shares.
struct Shared {
    config: Config,
    /// The authority's channel to the keeper.
    keeper: Keeper,
    /// The egress proxy's HTTPS roots.
    tls: TlsClient,
    /// The search provider, while search is on.
    provider: Option<Provider>,
    /// How long a search may take.
    deadline: Duration,
    /// Each harness's vendor, from the authority's own configuration.
    vendors: BTreeMap<HarnessKind, Vendor>,
}

/// Serves each connection the broker hands over on `broker`, on a thread
/// of its own, until the broker goes.
fn serve(broker: &UnixStream, shared: &Arc<Shared>) {
    while let Ok(fd) = hatchling_sys::net::recv_fd(broker.as_fd()) {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let stream = UnixStream::from(fd);
            if let Err(error) = serve_connection(&stream, &shared) {
                tracing::warn!(%error, "a connection from the broker ended");
            }
        });
    }
}

/// Answers one connection until the broker closes it.
fn serve_connection(stream: &UnixStream, shared: &Shared) -> Result<(), Error> {
    let mut proxy = None;
    let mut relay = Relaying::None;
    while let Some(frame) = read_frame_or_eof(stream).map_err(gone)? {
        let request: Request = serde_json::from_slice(&frame).map_err(|e| Error::Authority {
            reason: format!("a request that doesn't parse: {e}"),
        })?;
        let reply = match request {
            Request::Search { query } => match search(shared, &query) {
                Ok(hits) => Reply::Results {
                    hits: hits
                        .into_iter()
                        .map(|hit| Found {
                            title: hit.title,
                            url: hit.url.to_string(),
                            snippet: hit.snippet,
                        })
                        .collect(),
                },
                Err(reason) => Reply::Refused { reason },
            },
            Request::Egress => match open_proxy(shared) {
                Ok(opened) => {
                    proxy = Some(opened);
                    Reply::Ready
                }
                Err(error) => Reply::Failed {
                    reason: error.to_string(),
                },
            },
            Request::Mint {
                peer_uid,
                destination,
                ttl_secs,
            } => match granted(shared, proxy.as_ref(), &destination) {
                Ok(proxy) => match EgressPath::mint(proxy, peer_uid, &destination, ttl_secs) {
                    Ok(surrogate_b64) => Reply::Surrogate { surrogate_b64 },
                    Err(refusal) => Reply::Denied { refusal },
                },
                Err(refusal) => Reply::Denied { refusal },
            },
            Request::Forward {
                peer_uid,
                method,
                destination,
                path,
                headers,
                surrogate_b64,
                parts,
            } => {
                let body = read_parts(stream, parts, MAX_REQUEST_BYTES)?;
                let request = EgressRequest {
                    method,
                    destination,
                    path,
                    headers,
                    body,
                    surrogate_b64,
                };
                match granted(shared, proxy.as_ref(), &request.destination)
                    .and_then(|proxy| EgressPath::forward(proxy, peer_uid, &request))
                {
                    Ok(answer) => {
                        answer_with(stream, answer)?;
                        continue;
                    }
                    Err(refusal) => Reply::Denied { refusal },
                }
            }
            Request::Relay { harness, surrogate } => {
                let (reply, ready) = ready_relay(shared, &harness, surrogate);
                relay = ready.map_or(Relaying::None, Relaying::Ready);
                reply
            }
            Request::Start { seconds } => start_relay(stream, &mut relay, seconds)?,
            Request::Finish => finish_relay(&mut relay),
        };
        send(stream, &reply)?;
    }
    Ok(())
}

/// Starts the relay made ready, serving for `seconds`, with the handoff
/// socket that follows on `stream`. The socket is taken whatever the
/// relay's state, so the stream stays in step.
fn start_relay(stream: &UnixStream, relay: &mut Relaying, seconds: u64) -> Result<Reply, Error> {
    let handoff = hatchling_sys::net::recv_fd(stream.as_fd()).map_err(gone)?;
    Ok(match std::mem::replace(relay, Relaying::None) {
        Relaying::Ready(ready) => {
            let serving = Duration::from_secs(seconds).min(MAX_RELAY);
            *relay = Relaying::Serving(Relay::start(
                UnixListener::from(handoff),
                ready.vendor,
                ready.surrogate,
                ready.key,
                Instant::now() + serving,
            ));
            Reply::Ready
        }
        Relaying::None | Relaying::Serving(_) => Reply::Failed {
            reason: String::from("no relay was made ready"),
        },
    })
}

/// Stops the relay started, and says what it did.
fn finish_relay(relay: &mut Relaying) -> Reply {
    match std::mem::replace(relay, Relaying::None) {
        Relaying::Serving(serving) => {
            let (count, problem) = serving.finish();
            Reply::Relayed {
                forwarded: count.forwarded,
                refused: count.refused,
                problem,
            }
        }
        Relaying::None | Relaying::Ready(_) => Reply::Failed {
            reason: String::from("no relay was started"),
        },
    }
}

/// A connection's harness relay, as far as it has got.
enum Relaying {
    None,
    Ready(ReadyRelay),
    Serving(Relay),
}

/// A relay made ready: its vendor, the surrogate the cell holds, and the
/// vendor key.
struct ReadyRelay {
    vendor: Vendor,
    surrogate: Zeroizing<String>,
    key: Zeroizing<String>,
}

/// Makes ready a relay for the harness `harness` names: its vendor from
/// the authority's own configuration, its key from the keeper. The reply,
/// and the relay when it's ready.
fn ready_relay(shared: &Shared, harness: &str, surrogate: String) -> (Reply, Option<ReadyRelay>) {
    let refused = |reason: String| (Reply::Refused { reason }, None);
    let Some(kind) = HarnessKind::ALL
        .into_iter()
        .find(|kind| kind.name() == harness)
    else {
        return refused(format!("{harness:?} is not a harness"));
    };
    let Some(vendor) = shared.vendors.get(&kind) else {
        return refused(format!(
            "the {harness} harness has no vendor API configured"
        ));
    };
    let no_key = || {
        refused(format!(
            "the vendor key is not in the vault: store it as the secret {}",
            kind.secret_name()
        ))
    };
    let Ok(name) = kind.secret_name().parse::<SecretName>() else {
        return no_key();
    };
    let key = shared
        .keeper
        .read("harness", std::slice::from_ref(&name))
        .ok()
        .and_then(|mut read| read.pop().flatten())
        .and_then(|secret| crate::harness::key_text(&secret));
    let Some(key) = key else {
        return no_key();
    };
    let ready = ReadyRelay {
        vendor: vendor.clone(),
        surrogate: Zeroizing::new(surrogate),
        key,
    };
    (Reply::Ready, Some(ready))
}

/// The connection's proxy, for `destination` one of the operator's
/// grants: a broker can't send the authority anywhere no agent may go.
fn granted<'p, P: EgressPath>(
    shared: &Shared,
    proxy: Option<&'p P>,
    destination: &str,
) -> Result<&'p P, Refusal> {
    let proxy = proxy.ok_or_else(|| Refusal::failed("this connection's egress isn't open"))?;
    if shared
        .config
        .egress
        .grants
        .iter()
        .any(|grant| grant == destination)
    {
        Ok(proxy)
    } else {
        tracing::warn!(
            destination = ?destination,
            "refused a destination no agent is granted"
        );
        Err(Refusal {
            kind: RefusalKind::NotAllowed,
            reason: String::from("no agent is granted this destination"),
        })
    }
}

/// Sends `answer`: its status and headers, then its body in parts. One
/// whose headers don't fit a frame is refused instead.
fn answer_with(stream: &UnixStream, answer: EgressResponse) -> Result<(), Error> {
    let header = Reply::Answer {
        status: answer.status,
        headers: answer.headers,
        parts: answer.body.chunks(PART_BYTES).count(),
    };
    if !fits_one_frame(&header) {
        return send(
            stream,
            &Reply::Denied {
                refusal: Refusal::failed("the answer's headers don't fit a frame"),
            },
        );
    }
    send(stream, &header)?;
    for part in answer.body.chunks(PART_BYTES) {
        write_frame(stream, part).map_err(gone)?;
    }
    Ok(())
}

/// A session's proxy: a fresh surrogate key, over the allowlist resolved
/// once for the authority's life, with each credential read from the
/// keeper for the request that needs it.
///
/// # Errors
///
/// Returns [`Error::Egress`] when the allowlist doesn't resolve, or the
/// surrogate key can't be made.
#[allow(
    clippy::type_complexity,
    reason = "the credential-lookup closure type is inherent to the proxy's generic design"
)]
fn open_proxy(
    shared: &Shared,
) -> Result<Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>> + '_>, Error> {
    let allowlist = shared
        .config
        .egress
        .allowlist
        .resolve()
        .map_err(|source| Error::Egress { source })?;
    let keeper = &shared.keeper;
    Proxy::shared(allowlist, shared.tls.clone(), move |destination: &str| {
        let name = destination_to_secret_name(destination)
            .parse::<SecretName>()
            .ok()?;
        let secret = keeper
            .read("egress", std::slice::from_ref(&name))
            .ok()?
            .pop()??;
        Some(Zeroizing::new(secret.as_bytes().to_vec()))
    })
    .map_err(|source| Error::Egress { source })
}

/// Searches for `query` with the provider's key, read from the keeper,
/// giving up after the shared deadline.
fn search(shared: &Shared, query: &str) -> Result<Vec<Hit>, String> {
    let Some(provider) = shared
        .provider
        .as_ref()
        .filter(|_| shared.config.web.search)
    else {
        return Err(String::from("search is off"));
    };
    let name: SecretName = SEARCH_KEY
        .parse()
        .map_err(|_| String::from("the search key's name is invalid"))?;
    let read = shared
        .keeper
        .read("search", std::slice::from_ref(&name))
        .map_err(|error| format!("the search key couldn't be read: {error}"))?;
    let secret =
        read.into_iter().next().flatten().ok_or_else(|| {
            format!("no search key is stored: keep it in the vault as {SEARCH_KEY}")
        })?;
    let key = Zeroizing::new(secret.as_bytes().to_vec());
    let (sender, receiver) = mpsc::channel();
    let provider = Arc::clone(provider);
    let query = query.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(provider(&key, &query));
    });
    receiver.recv_timeout(shared.deadline).map_or_else(
        |_| {
            Err(format!(
                "the search took longer than {} seconds",
                FETCH_DEADLINE.as_secs()
            ))
        },
        |answer| answer.map_err(|error| error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use super::*;

    /// The broker waits for the authority's word: an empty frame is
    /// started, any other is why not, and an authority gone said nothing.
    #[test]
    fn the_broker_hears_whether_the_authority_started() {
        let said = |word: Option<&[u8]>| {
            let (ours, theirs) = UnixStream::pair().unwrap();
            if let Some(word) = word {
                write_frame(&theirs, word).unwrap();
            }
            drop(theirs);
            Authority::over(ours).started()
        };
        said(Some(b"")).unwrap();
        let refused = said(Some(b"cannot read the CA bundle")).unwrap_err();
        assert!(
            refused.to_string().contains("cannot read the CA bundle"),
            "{refused}"
        );
        let gone = said(None).unwrap_err();
        assert!(gone.to_string().contains("didn't start"), "{gone}");
        // Not as root, the authority is the broker's own user.
        if hatchling_sys::euid() != 0 {
            assert!(egress_user().unwrap().is_none());
        }
    }

    /// The purposes a fake keeper was asked for, in order.
    type Asked = Arc<StdMutex<Vec<String>>>;

    /// A credential lookup, as a plain function.
    type Fetcher = fn(&str) -> Option<Zeroizing<Vec<u8>>>;

    /// A keeper on the other end of a pair: it answers each read with the
    /// value it holds for the read's purpose, or with nothing stored, and
    /// records each request's purpose.
    fn keeper_holding(values: &'static [(&'static str, &'static [u8])]) -> (Keeper, Asked) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let asked = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame_or_eof(&theirs) {
                let request: serde_json::Value = serde_json::from_slice(&frame).unwrap();
                let purpose = request
                    .get("purpose")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let value = values
                    .iter()
                    .find(|(held, _)| *held == purpose)
                    .map(|(_, value)| *value);
                log.lock().unwrap().push(purpose);
                let found = serde_json::json!({"reply": "read", "found": [value.is_some()]});
                write_frame(&theirs, &serde_json::to_vec(&found).unwrap()).unwrap();
                if let Some(value) = value {
                    write_frame(&theirs, value).unwrap();
                }
            }
        });
        (Keeper::over(ours).unwrap(), asked)
    }

    fn shared_with(
        config: Config,
        values: &'static [(&'static str, &'static [u8])],
        provider: Option<Provider>,
        deadline: Duration,
    ) -> (Shared, Asked) {
        let (keeper, asked) = keeper_holding(values);
        let shared = Shared {
            config,
            keeper,
            tls: TlsClient::bundled().unwrap(),
            provider,
            deadline,
            vendors: BTreeMap::new(),
        };
        (shared, asked)
    }

    fn searching() -> Config {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.search = true;
        config
    }

    /// A config whose egress allows and grants `destination`, with
    /// `allowlist` its allowlist.
    fn egressing(allowlist: &str, grants: &[&str]) -> Config {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.egress = crate::EgressConfig {
            allowlist: crate::EgressAllowlist::new(allowlist),
            grants: grants.iter().map(|grant| (*grant).to_owned()).collect(),
        };
        config
    }

    /// An authority serving `shared` over a pair, and the broker's end.
    fn serving(shared: Shared) -> (Authority, std::thread::JoinHandle<()>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let shared = Arc::new(shared);
        let served = std::thread::spawn(move || serve(&theirs, &shared));
        (Authority::over(ours), served)
    }

    fn hit(url: &str) -> Hit {
        Hit {
            title: String::from("A"),
            url: WebUrl::parse(url).unwrap(),
            snippet: String::from("about A"),
        }
    }

    #[test]
    fn a_search_runs_with_the_key_the_keeper_releases_to_the_authority() {
        let given = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&given);
        let provider: Provider = Arc::new(move |key, query| {
            log.lock().unwrap().push((key.to_vec(), query.to_owned()));
            Ok(vec![hit("https://docs.example/a")])
        });
        let (shared, asked) = shared_with(
            searching(),
            &[("search", b"k-brave")],
            Some(provider),
            FETCH_DEADLINE,
        );
        assert_eq!(
            search(&shared, "rust"),
            Ok(vec![hit("https://docs.example/a")])
        );
        assert_eq!(
            *given.lock().unwrap(),
            [(b"k-brave".to_vec(), String::from("rust"))]
        );
        assert_eq!(*asked.lock().unwrap(), ["search"], "read for search alone");
    }

    #[test]
    fn a_search_is_off_or_refused_with_why() {
        let provider: Provider = Arc::new(|_, _| Ok(Vec::new()));
        let (shared, asked) = shared_with(searching(), &[], Some(provider), FETCH_DEADLINE);
        assert_eq!(
            search(&shared, "q"),
            Err(format!(
                "no search key is stored: keep it in the vault as {SEARCH_KEY}"
            ))
        );
        let (unprovided, _) = shared_with(searching(), &[], None, FETCH_DEADLINE);
        assert_eq!(
            search(&unprovided, "q"),
            Err(String::from("search is off")),
            "no provider"
        );
        let provider: Provider = Arc::new(|_, _| Ok(Vec::new()));
        let mut off = shared;
        off.config.web.search = false;
        off.provider = Some(provider);
        assert_eq!(search(&off, "q"), Err(String::from("search is off")));
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "no key read while search is off"
        );
        let failing: Provider = Arc::new(|_, _| Err(SearchError::Status { status: 429 }));
        let (failing, _) = shared_with(
            searching(),
            &[("search", b"k")],
            Some(failing),
            FETCH_DEADLINE,
        );
        assert_eq!(
            search(&failing, "q"),
            Err(SearchError::Status { status: 429 }.to_string())
        );
        let slow: Provider = Arc::new(|_, _| {
            std::thread::sleep(Duration::from_secs(3));
            Ok(Vec::new())
        });
        let (slow, _) = shared_with(
            searching(),
            &[("search", b"k")],
            Some(slow),
            Duration::from_millis(100),
        );
        let started = Instant::now();
        assert_eq!(
            search(&slow, "q"),
            Err(format!(
                "the search took longer than {} seconds",
                FETCH_DEADLINE.as_secs()
            ))
        );
        assert!(started.elapsed() < Duration::from_secs(3), "waited for it");
        // A keeper that's gone: the key couldn't be read.
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let provider: Provider = Arc::new(|_, _| Ok(Vec::new()));
        let gone_keeper = Shared {
            config: searching(),
            keeper: Keeper::over(ours).unwrap(),
            tls: TlsClient::bundled().unwrap(),
            provider: Some(provider),
            deadline: FETCH_DEADLINE,
            vendors: BTreeMap::new(),
        };
        assert!(matches!(
            search(&gone_keeper, "q"),
            Err(reason) if reason.starts_with("the search key couldn't be read")
        ));
    }

    /// Each connection is served on its own: a session's egress opens and
    /// mints while a slow search runs on another, and the authority ends
    /// when the broker goes.
    #[test]
    fn each_connection_is_served_on_its_own() {
        let slow: Provider = Arc::new(|_, query| {
            std::thread::sleep(Duration::from_secs(2));
            Ok(vec![hit(&format!("https://docs.example/{query}"))])
        });
        let mut config = egressing("internal:localhost:443", &["localhost:443"]);
        config.web.search = true;
        let (shared, _) = shared_with(config, &[("search", b"k")], Some(slow), FETCH_DEADLINE);
        let (authority, served) = serving(shared);
        let authority = Arc::new(authority);
        let searcher = Arc::clone(&authority);
        let searching = std::thread::spawn(move || searcher.search("rust"));
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        let link = authority.connect().unwrap().egress().unwrap();
        assert!(link.mint(1000, "localhost:443", 60).is_ok());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the egress waited on the search"
        );
        assert_eq!(
            searching.join().unwrap(),
            Ok(vec![hit("https://docs.example/rust")])
        );
        drop(link);
        drop(authority);
        served.join().unwrap();
    }

    /// A one-request upstream on loopback: it records what it was sent,
    /// and answers with `reply`.
    fn upstream(reply: Vec<u8>) -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut seen = Vec::new();
            let mut byte = [0_u8; 1];
            while !seen.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                seen.extend_from_slice(&byte);
            }
            let head = String::from_utf8_lossy(&seen).into_owned();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            seen.extend_from_slice(&body);
            stream.write_all(&reply).unwrap();
            seen
        });
        (port, handle)
    }

    /// A session's request goes through the authority whole: its body in
    /// parts, the destination's credential put in from the keeper, and
    /// the answer back in parts with the credential scrubbed from it.
    #[test]
    fn a_request_goes_through_the_authority_with_its_credential_put_in() {
        let mut answer_body = vec![b'a'; 150 * 1024];
        answer_body.extend_from_slice(b"echo: Bearer cred-zq-7");
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            answer_body.len()
        )
        .into_bytes();
        reply.extend_from_slice(&answer_body);
        let (port, upstream) = upstream(reply);
        let destination = format!("localhost:{port}");
        let config = egressing(&format!("local:{destination}"), &[&destination]);
        let (shared, asked) =
            shared_with(config, &[("egress", b"cred-zq-7")], None, FETCH_DEADLINE);
        let (authority, served) = serving(shared);
        let link = authority.connect().unwrap().egress().unwrap();
        let surrogate_b64 = link.mint(1000, &destination, 60).unwrap();
        let body: Vec<u8> = (0..130 * 1024_u32)
            .map(|n| b'x' + u8::try_from(n % 3).unwrap())
            .collect();
        let request = EgressRequest {
            method: String::from("POST"),
            destination,
            path: String::from("/v1/chat"),
            headers: vec![(String::from("X-Planted"), String::from("p-7"))],
            body: body.clone(),
            surrogate_b64,
        };
        let answer = link.forward(1000, &request).unwrap();
        let seen = upstream.join().unwrap();
        let seen_text = String::from_utf8_lossy(&seen);
        assert!(
            seen_text.contains("\r\nAuthorization: Bearer cred-zq-7\r\n"),
            "the credential wasn't put in"
        );
        assert!(seen_text.contains("X-Planted: p-7"));
        assert!(seen.ends_with(&body), "the body didn't arrive whole");
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body.len(), answer_body.len());
        assert!(
            answer
                .body
                .starts_with(answer_body.get(..150 * 1024).unwrap())
        );
        assert!(
            !String::from_utf8_lossy(&answer.body).contains("cred-zq-7"),
            "the credential came back"
        );
        assert_eq!(*asked.lock().unwrap(), ["egress"]);
        drop(link);
        drop(authority);
        served.join().unwrap();
    }

    /// The authority refuses what the proxy would, and what no grant
    /// allows, whatever the broker asks; a connection whose egress isn't
    /// open mints nothing; an allowlist that doesn't resolve opens none.
    #[test]
    fn the_authority_refuses_what_no_grant_or_proxy_allows() {
        let config = egressing(
            "internal:localhost:443,internal:localhost:8443",
            &["localhost:443"],
        );
        let (shared, asked) = shared_with(config, &[("egress", b"c")], None, FETCH_DEADLINE);
        let (authority, served) = serving(shared);
        // Not opened: nothing minted.
        let unopened = authority.connect().unwrap();
        send(
            &unopened.stream,
            &Request::Mint {
                peer_uid: 1000,
                destination: String::from("localhost:443"),
                ttl_secs: 60,
            },
        )
        .unwrap();
        assert!(matches!(
            receive(&unopened.stream).unwrap(),
            Reply::Denied { refusal } if refusal.kind == RefusalKind::Failed
        ));
        let link = authority.connect().unwrap().egress().unwrap();
        // On the allowlist, granted to no agent: refused by the authority.
        let ungranted = link.mint(1000, "localhost:8443", 60).unwrap_err();
        assert_eq!(ungranted.kind, RefusalKind::NotAllowed);
        assert_eq!(ungranted.reason, "no agent is granted this destination");
        // A surrogate for another peer, or a forged one: refused.
        let surrogate_b64 = link.mint(1000, "localhost:443", 60).unwrap();
        let request = |surrogate_b64: String| EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64,
        };
        let refused = link.forward(1001, &request(surrogate_b64)).unwrap_err();
        assert_eq!(refused.kind, RefusalKind::BadSurrogate);
        let forged = link
            .forward(1000, &request(String::from("c3Vycm9nYXRl")))
            .unwrap_err();
        assert_eq!(forged.kind, RefusalKind::BadSurrogate);
        assert!(asked.lock().unwrap().is_empty(), "a credential was read");
        drop((unopened, link, authority));
        served.join().unwrap();
        // An allowlist that doesn't parse: the session's egress isn't
        // opened, and says why.
        let (shared, _) = shared_with(
            egressing("*.example.com:443", &[]),
            &[],
            None,
            FETCH_DEADLINE,
        );
        let (authority, served) = serving(shared);
        assert!(matches!(
            authority.connect().unwrap().egress(),
            Err(Error::Authority { .. })
        ));
        drop(authority);
        served.join().unwrap();
    }

    /// A link answered out of step is lost: nothing more is sent on it.
    /// A connection the authority can't be handed loses the authority.
    #[test]
    fn a_link_or_an_authority_that_breaks_off_is_lost() {
        let (broker, theirs) = UnixStream::pair().unwrap();
        let stand_in = std::thread::spawn(move || {
            let _ = read_frame(&theirs);
            write_frame(&theirs, br#"{"reply":"ready"}"#).unwrap();
            let _ = read_frame(&theirs);
            write_frame(&theirs, br#"{"reply":"ready"}"#).unwrap();
            read_frame_or_eof(&theirs).unwrap()
        });
        let link = Connection::over(broker).unwrap().egress().unwrap();
        let unavailable = Err(Refusal::failed(UNAVAILABLE));
        assert_eq!(link.mint(1000, "localhost:443", 60), unavailable);
        assert_eq!(link.mint(1000, "localhost:443", 60), unavailable, "lost");
        drop(link);
        assert!(
            stand_in.join().unwrap().is_none(),
            "a request was sent after it was lost"
        );
        // An egress answered out of step isn't opened.
        let (broker, theirs) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let _ = read_frame(&theirs);
            write_frame(&theirs, br#"{"reply":"surrogate","surrogate_b64":"x"}"#).unwrap();
        });
        assert!(Connection::over(broker).unwrap().egress().is_err());
        // An authority that's gone: no connection is handed over, ever.
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let authority = Authority::over(ours);
        assert!(authority.connect().is_err());
        let lost = authority.connect().unwrap_err().to_string();
        assert!(lost.contains("lost"), "{lost}");
        assert_eq!(authority.search("q"), Err(String::from(UNAVAILABLE)));
    }

    /// A search's answer is checked: a bad url isn't trusted, and anything
    /// but results or a refusal is the authority being unavailable.
    #[test]
    fn a_searchs_answer_is_checked() {
        let answered = |answer: &'static [u8]| {
            let (broker, theirs) = UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                let _ = read_frame(&theirs);
                write_frame(&theirs, answer).unwrap();
            });
            Connection::over(broker).unwrap().search("q")
        };
        assert_eq!(
            answered(
                br#"{"reply":"results","hits":[{"title":"x","url":"http://plain.example/","snippet":"y"}]}"#
            ),
            Err(String::from("the egress authority answered a bad result"))
        );
        assert_eq!(
            answered(br#"{"reply":"refused","reason":"search is off"}"#),
            Err(String::from("search is off"))
        );
        assert_eq!(
            answered(br#"{"reply":"ready"}"#),
            Err(String::from(UNAVAILABLE))
        );
        assert_eq!(answered(b"not json"), Err(String::from(UNAVAILABLE)));
    }

    /// A body moves in parts of at most `PART_BYTES`, to its bound; more
    /// parts, a larger part, or more bytes are refused. An answer whose
    /// headers don't fit a frame is refused whole.
    #[test]
    fn bodies_move_in_bounded_parts() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        for part in [&b"ab"[..], b"cd"] {
            write_frame(&ours, part).unwrap();
        }
        assert_eq!(read_parts(&theirs, 2, PART_BYTES + 4).unwrap(), b"abcd");
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // More parts than the bound allows, however small: refused before
        // any is read.
        for _ in 0..3 {
            write_frame(&ours, b"").unwrap();
        }
        assert!(
            read_parts(&theirs, 3, PART_BYTES).is_err(),
            "too many parts"
        );
        for _ in 0..3 {
            assert_eq!(read_frame(&theirs).unwrap(), b"", "a part was read");
        }
        write_frame(&ours, &vec![0; PART_BYTES + 1]).unwrap();
        assert!(
            read_parts(&theirs, 1, 4 * PART_BYTES).is_err(),
            "a part too large"
        );
        write_frame(&ours, b"abcde").unwrap();
        assert!(
            read_parts(&theirs, 1, 4).is_err(),
            "more bytes than the bound"
        );
        assert_eq!(read_parts(&theirs, 0, 0).unwrap(), b"");
        // Headers too large for a frame: refused, nothing else sent.
        let answer = EgressResponse {
            status: 200,
            headers: vec![(String::from("X-Big"), "v".repeat(70 * 1024))],
            body: b"body".to_vec(),
        };
        answer_with(&ours, answer).unwrap();
        drop(ours);
        assert!(matches!(
            receive(&theirs).unwrap(),
            Reply::Denied { refusal } if refusal == Refusal::failed("the answer's headers don't fit a frame")
        ));
        assert!(read_frame_or_eof(&theirs).unwrap().is_none());
    }

    /// A request that doesn't parse ends its connection, and only it.
    #[test]
    fn a_request_that_does_not_parse_ends_its_connection() {
        let (shared, _) = shared_with(searching(), &[], None, FETCH_DEADLINE);
        let (broker, theirs) = UnixStream::pair().unwrap();
        write_frame(&broker, br#"{"request":"steal"}"#).unwrap();
        assert!(serve_connection(&theirs, &shared).is_err());
        let (broker, theirs) = UnixStream::pair().unwrap();
        drop(broker);
        assert!(serve_connection(&theirs, &shared).is_ok());
        assert!(gone("x").to_string().contains("cannot be reached: x"));
    }

    /// `open_proxy` and `granted`, called directly.
    #[test]
    fn a_sessions_proxy_is_its_own_and_reaches_only_grants() {
        let (shared, _) = shared_with(
            egressing("internal:localhost:443", &["localhost:443"]),
            &[],
            None,
            FETCH_DEADLINE,
        );
        let first = open_proxy(&shared).unwrap();
        let second = open_proxy(&shared).unwrap();
        assert!(std::ptr::eq(first.allowlist(), second.allowlist()));
        let surrogate_b64 = first.mint_surrogate(1000, "localhost:443", 60).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64,
        };
        assert!(matches!(
            second.forward(1000, &request),
            Err(hatchling_egress::Error::InvalidSurrogate)
        ));
        assert!(granted(&shared, Some(&first), "localhost:443").is_ok());
        assert_eq!(
            granted(&shared, Some(&first), "elsewhere.example:443")
                .map(|_| ())
                .unwrap_err()
                .kind,
            RefusalKind::NotAllowed
        );
        let none: Option<&Proxy<Fetcher>> = None;
        assert_eq!(
            granted(&shared, none, "localhost:443")
                .map(|_| ())
                .unwrap_err()
                .kind,
            RefusalKind::Failed
        );
    }

    /// A vendor upstream for one request: it answers `ok`, and returns
    /// what it was sent.
    fn vendor_once() -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut seen = Vec::new();
            let mut byte = [0_u8; 1];
            while !seen.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                seen.extend_from_slice(&byte);
            }
            let mut body = [0_u8; 2];
            stream.read_exact(&mut body).unwrap();
            seen.extend_from_slice(&body);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            String::from_utf8(seen).unwrap()
        });
        (port, handle)
    }

    /// A harness run's relay serves in the authority: the vendor from its
    /// own configuration, the key from the keeper, the cell's surrogate
    /// swapped for it, and the count back to the broker. A harness with no
    /// vendor, or no key stored, is refused before it starts.
    #[test]
    fn a_harness_runs_relay_serves_in_the_authority_with_the_key_it_reads() {
        let (port, vendor) = vendor_once();
        let destination = format!("localhost:{port}");
        let allowlist = hatchling_egress::Allowlist::parse(&format!("local:{destination}"));
        let (mut shared, asked) = shared_with(
            crate::api_auth::tests::owner_config(None),
            &[("harness", b" sk-real-zq-5\n")],
            None,
            FETCH_DEADLINE,
        );
        shared.vendors.insert(
            HarnessKind::ClaudeCode,
            Vendor {
                wire: crate::harness_relay::Wire::Anthropic,
                destination,
                allowlist: Arc::new(allowlist.unwrap()),
                tls: TlsClient::bundled().unwrap(),
            },
        );
        let (authority, served) = serving(shared);
        assert_eq!(
            authority
                .connect()
                .unwrap()
                .relay(HarnessKind::CodexCli, "s")
                .unwrap()
                .unwrap_err(),
            "the codex-cli harness has no vendor API configured"
        );
        let pending = authority
            .connect()
            .unwrap()
            .relay(HarnessKind::ClaudeCode, "sk-surrogate-zq")
            .unwrap()
            .unwrap();
        let dir = crate::api_auth::tests::scratch("authority-relay");
        let handoff_path = dir.0.join("relay.sock");
        let handoff = UnixListener::bind(&handoff_path).unwrap();
        let relay = pending.start(&handoff, Duration::from_secs(30)).unwrap();
        // The cell hands its listener over; the CLI asks through it.
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let cell_port = cell.local_addr().unwrap().port();
        let to_handoff = UnixStream::connect(&handoff_path).unwrap();
        hatchling_sys::net::send_fd(to_handoff.as_fd(), cell.as_fd()).unwrap();
        let mut cli = std::net::TcpStream::connect(("127.0.0.1", cell_port)).unwrap();
        cli.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        cli.write_all(
            b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-surrogate-zq\r\nContent-Length: 2\r\n\r\n{}",
        )
        .unwrap();
        let mut answer = String::new();
        cli.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.ends_with("ok"), "{answer}");
        let seen = vendor.join().unwrap();
        assert!(seen.contains("x-api-key: sk-real-zq-5\r\n"), "{seen}");
        assert!(!seen.contains("sk-surrogate-zq"), "{seen}");
        assert_eq!(
            relay.finish(),
            (
                RelayCount {
                    forwarded: 1,
                    refused: 0
                },
                None
            )
        );
        assert_eq!(*asked.lock().unwrap(), ["harness"]);
        drop((to_handoff, cell, authority));
        served.join().unwrap();
    }

    /// A relay for a harness that isn't one, or whose key isn't stored, is
    /// refused, in words for the run's report.
    #[test]
    fn a_relay_is_refused_without_its_harness_or_its_key() {
        let (mut shared, _) = shared_with(
            crate::api_auth::tests::owner_config(None),
            &[],
            None,
            FETCH_DEADLINE,
        );
        let allowlist = hatchling_egress::Allowlist::parse("local:localhost:1").unwrap();
        shared.vendors.insert(
            HarnessKind::ClaudeCode,
            Vendor {
                wire: crate::harness_relay::Wire::Anthropic,
                destination: String::from("localhost:1"),
                allowlist: Arc::new(allowlist),
                tls: TlsClient::bundled().unwrap(),
            },
        );
        let (refused, _) = ready_relay(&shared, "claude-code", String::from("s"));
        assert!(matches!(
            refused,
            Reply::Refused { reason } if reason == "the vendor key is not in the vault: store it as the secret harness-claude-code"
        ));
        let (unknown, none) = ready_relay(&shared, "vim", String::from("s"));
        assert!(none.is_none());
        assert!(
            matches!(unknown, Reply::Refused { reason } if reason == "\"vim\" is not a harness")
        );
    }

    /// A relay is started only once made ready, and finished only once
    /// started; the handoff socket is taken either way, so the
    /// connection stays in step.
    #[test]
    fn a_relay_runs_its_steps_in_order() {
        let (shared, _) = shared_with(searching(), &[], None, FETCH_DEADLINE);
        let (authority, served) = serving(shared);
        let connection = authority.connect().unwrap();
        let dir = crate::api_auth::tests::scratch("authority-relay-order");
        let handoff = UnixListener::bind(dir.0.join("relay.sock")).unwrap();
        let unready = PendingRelay { connection };
        assert!(matches!(
            unready.start(&handoff, Duration::from_secs(1)),
            Err(Error::Authority { reason }) if reason == "no relay was made ready"
        ));
        let unstarted = AuthorityRelay {
            connection: authority.connect().unwrap(),
        };
        let (count, problem) = unstarted.finish();
        assert_eq!(count, RelayCount::default());
        assert!(problem.is_some_and(|problem| problem.contains("no relay was started")));
        assert!(matches!(
            finish_relay(&mut Relaying::None),
            Reply::Failed { reason } if reason == "no relay was started"
        ));
        assert!(
            out_of_step()
                .to_string()
                .contains("it answered out of step")
        );
        // Started with nothing made ready: the handoff is taken all the
        // same, and refused.
        let (ours, theirs) = UnixStream::pair().unwrap();
        hatchling_sys::net::send_fd(ours.as_fd(), handoff.as_fd()).unwrap();
        assert!(matches!(
            start_relay(&theirs, &mut Relaying::None, 1).unwrap(),
            Reply::Failed { reason } if reason == "no relay was made ready"
        ));
        drop(authority);
        served.join().unwrap();
    }

    #[test]
    fn the_authority_runs_only_as_the_broker_starts_it() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", egress_authority_main()), failure);
        assert_eq!(format!("{:?}", refuse_to_run("why")), failure);
    }
}
