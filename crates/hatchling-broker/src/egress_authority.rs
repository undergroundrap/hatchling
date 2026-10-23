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
//! **What it does now (slice 1): web search.** The broker sends a query;
//! the authority reads the search provider's key from the keeper, runs the
//! search with it, and answers with the results. The broker never holds
//! the key. The egress swap, the harness relay, and the rest move here
//! slice by slice (WO-017's notes).
//!
//! **What it holds against.** A compromised broker can't read a key the
//! authority holds: the keeper releases it to the authority's channel
//! alone, and the authority isn't dumpable. It is not yet an
//! operating-system boundary: the broker starts it as its own user, and a
//! root broker can reach any process. The spawner split gives each its own
//! user.

use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use hatchling_core::SecretName;
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_egress::search::{Hit, SearchError};
use hatchling_egress::web::WebUrl;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::keeper::Keeper;
use crate::web_ops::{FETCH_DEADLINE, SEARCH_KEY};
use crate::{Config, Error};

/// The authority's subcommand: `hatchling-broker egress-authority`,
/// started only by the broker.
pub const EGRESS_AUTHORITY_SUBCOMMAND: &str = "egress-authority";

/// The setting that names the authority's end of its pair with the broker.
const BROKER_FD: &str = crate::environment::HATCHLING_AUTHORITY_FD.name;

/// The setting that names the authority's end of its pair with the keeper.
const KEEPER_FD: &str = crate::environment::HATCHLING_AUTHORITY_KEEPER_FD.name;

/// How long the broker waits for each of the authority's answers: past the
/// search's own deadline, which the authority keeps.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(FETCH_DEADLINE.as_secs() * 2);

/// Runs one search with the provider's key.
pub type Provider = Arc<dyn Fn(&[u8], &str) -> Result<Vec<Hit>, SearchError> + Send + Sync>;

/// What the broker asks the authority.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    /// A web search for `query`.
    Search { query: String },
}

/// What the authority answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    /// A search's results, the provider's words.
    Results { hits: Vec<Found> },
    /// Refused, and why: the broker's to tell the agent.
    Refused { reason: String },
}

/// One search result, as it crosses to the broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Found {
    title: String,
    url: String,
    snippet: String,
}

/// The broker's end of the authority: one request at a time.
#[derive(Debug)]
pub struct Authority {
    stream: Mutex<UnixStream>,
    /// Set once an exchange broke off: no request is sent again.
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
    /// broker, and `keeper`, its end of the pair with the keeper.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Authority`] when it can't be started.
    pub fn start_with(mut program: Command, keeper: &UnixStream) -> Result<Self, Error> {
        let failed = |reason: String| Error::Authority { reason };
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
        Ok(Self {
            _child: Some(child),
            ..Self::over(ours)?
        })
    }

    /// The broker's end, `stream`, of an authority already running.
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
        Ok(Self {
            stream: Mutex::new(stream),
            lost: AtomicBool::new(false),
            _child: None,
        })
    }

    /// Searches the web for `query`, through the authority: the results,
    /// or why there are none.
    ///
    /// # Errors
    ///
    /// A sentence for the agent: the authority refused, or can't be
    /// reached.
    pub fn search(&self, query: &str) -> Result<Vec<Hit>, String> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| String::from("the egress authority is unavailable"))?;
        if self.lost.load(Ordering::SeqCst) {
            return Err(String::from("the egress authority is unavailable"));
        }
        let answer = ask(
            &stream,
            &Request::Search {
                query: query.to_owned(),
            },
        );
        drop(stream);
        match answer {
            Ok(Reply::Results { hits }) => hits
                .into_iter()
                .map(|found| {
                    WebUrl::parse(&found.url)
                        .map(|url| Hit {
                            title: found.title,
                            url,
                            snippet: found.snippet,
                        })
                        .map_err(|_| String::from("the egress authority answered a bad result"))
                })
                .collect(),
            Ok(Reply::Refused { reason }) => Err(reason),
            Err(error) => {
                self.lost.store(true, Ordering::SeqCst);
                tracing::error!(%error, "the egress authority can't be reached");
                Err(String::from("the egress authority is unavailable"))
            }
        }
    }
}

/// Sends `request`, and reads the reply.
fn ask(stream: &UnixStream, request: &Request) -> Result<Reply, Error> {
    let bytes = serde_json::to_vec(request).map_err(|e| Error::Authority {
        reason: format!("cannot write a request: {e}"),
    })?;
    write_frame(stream, &bytes).map_err(gone)?;
    let reply = read_frame(stream).map_err(gone)?;
    serde_json::from_slice(&reply).map_err(|e| Error::Authority {
        reason: format!("an answer that doesn't parse: {e}"),
    })
}

fn gone(error: impl std::fmt::Display) -> Error {
    Error::Authority {
        reason: format!("cannot be reached: {error}"),
    }
}

/// The authority: `hatchling-broker egress-authority`.
#[must_use]
pub fn egress_authority_main() -> ExitCode {
    let inherited = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|fd| fd.parse::<i32>().ok())
            .map(hatchling_sys::net::inherited_unix_stream)
    };
    let (broker, keeper) = match (inherited(BROKER_FD), inherited(KEEPER_FD)) {
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
            let provider = if config.web.search {
                let tls = crate::serve::egress_tls(|key| std::env::var(key).ok())?;
                let provider: Provider =
                    Arc::new(move |key, query| hatchling_egress::search::brave(&tls, key, query));
                Some(provider)
            } else {
                None
            };
            Keeper::over(keeper).map(|keeper| (config, keeper, provider))
        });
    let (config, keeper, provider) = match begun {
        Ok(begun) => begun,
        Err(error) => return refuse_to_run(&error.to_string()),
    };
    match serve(&broker, &keeper, &config, provider.as_ref(), FETCH_DEADLINE) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => refuse_to_run(&error.to_string()),
    }
}

fn refuse_to_run(reason: &str) -> ExitCode {
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "hatchling-broker egress-authority: {reason}"
    );
    ExitCode::FAILURE
}

/// Answers the broker on `broker` until it goes.
fn serve(
    broker: &UnixStream,
    keeper: &Keeper,
    config: &Config,
    provider: Option<&Provider>,
    deadline: Duration,
) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(broker).map_err(gone)? {
        let request: Request = serde_json::from_slice(&frame).map_err(|e| Error::Authority {
            reason: format!("a request that doesn't parse: {e}"),
        })?;
        let reply = match request {
            Request::Search { query } => match search(keeper, config, provider, &query, deadline) {
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
        };
        let bytes = serde_json::to_vec(&reply).map_err(|e| Error::Authority {
            reason: format!("cannot write an answer: {e}"),
        })?;
        write_frame(broker, &bytes).map_err(gone)?;
    }
    Ok(())
}

/// Searches for `query` with the provider's key, read from `keeper`, giving
/// up after `deadline`.
fn search(
    keeper: &Keeper,
    config: &Config,
    provider: Option<&Provider>,
    query: &str,
    deadline: Duration,
) -> Result<Vec<Hit>, String> {
    let Some(provider) = provider.filter(|_| config.web.search) else {
        return Err(String::from("search is off"));
    };
    let name: SecretName = SEARCH_KEY
        .parse()
        .map_err(|_| String::from("the search key's name is invalid"))?;
    let read = keeper
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
    receiver.recv_timeout(deadline).map_or_else(
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
    use std::sync::Mutex as StdMutex;

    use super::*;

    /// A keeper on the other end of `stream`: it answers each read with
    /// `key`, or with nothing stored when there's none, and records each
    /// request's purpose.
    fn keeper_answering(key: Option<&'static [u8]>) -> (Keeper, Arc<StdMutex<Vec<String>>>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let asked = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame_or_eof(&theirs) {
                let request: serde_json::Value = serde_json::from_slice(&frame).unwrap();
                let purpose = request.get("purpose").and_then(serde_json::Value::as_str);
                log.lock()
                    .unwrap()
                    .push(purpose.unwrap_or_default().to_owned());
                let found = serde_json::json!({"reply": "read", "found": [key.is_some()]});
                write_frame(&theirs, &serde_json::to_vec(&found).unwrap()).unwrap();
                if let Some(key) = key {
                    write_frame(&theirs, key).unwrap();
                }
            }
        });
        (Keeper::over(ours).unwrap(), asked)
    }

    fn searching() -> Config {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.web.search = true;
        config
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
        let (keeper, asked) = keeper_answering(Some(b"k-brave"));
        let given = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&given);
        let provider: Provider = Arc::new(move |key, query| {
            log.lock().unwrap().push((key.to_vec(), query.to_owned()));
            Ok(vec![hit("https://docs.example/a")])
        });
        let config = searching();
        assert_eq!(
            search(&keeper, &config, Some(&provider), "rust", FETCH_DEADLINE),
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
        let (keeper, asked) = keeper_answering(None);
        let mut config = searching();
        assert_eq!(
            search(&keeper, &config, Some(&provider), "q", FETCH_DEADLINE),
            Err(format!(
                "no search key is stored: keep it in the vault as {SEARCH_KEY}"
            ))
        );
        assert_eq!(
            search(&keeper, &config, None, "q", FETCH_DEADLINE),
            Err(String::from("search is off")),
            "no provider"
        );
        config.web.search = false;
        assert_eq!(
            search(&keeper, &config, Some(&provider), "q", FETCH_DEADLINE),
            Err(String::from("search is off"))
        );
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "no key read while search is off"
        );
        let config = searching();
        let (keeper, _) = keeper_answering(Some(b"k"));
        let failing: Provider = Arc::new(|_, _| Err(SearchError::Status { status: 429 }));
        assert_eq!(
            search(&keeper, &config, Some(&failing), "q", FETCH_DEADLINE),
            Err(SearchError::Status { status: 429 }.to_string())
        );
        let slow: Provider = Arc::new(|_, _| {
            std::thread::sleep(Duration::from_secs(3));
            Ok(Vec::new())
        });
        let started = std::time::Instant::now();
        assert_eq!(
            search(
                &keeper,
                &config,
                Some(&slow),
                "q",
                Duration::from_millis(100)
            ),
            Err(format!(
                "the search took longer than {} seconds",
                FETCH_DEADLINE.as_secs()
            ))
        );
        assert!(started.elapsed() < Duration::from_secs(3), "waited for it");
        // A keeper that's gone: the key couldn't be read.
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let gone_keeper = Keeper::over(ours).unwrap();
        assert!(matches!(
            search(&gone_keeper, &config, Some(&provider), "q", FETCH_DEADLINE),
            Err(reason) if reason.starts_with("the search key couldn't be read")
        ));
    }

    #[test]
    fn the_broker_is_answered_over_its_channel_and_learns_only_results_or_why() {
        let (keeper, _) = keeper_answering(Some(b"k-brave"));
        let provider: Provider = Arc::new(|key, query| {
            assert_eq!(key, b"k-brave");
            Ok(vec![hit(&format!("https://docs.example/{query}"))])
        });
        let (broker, theirs) = UnixStream::pair().unwrap();
        let config = searching();
        let serving = std::thread::spawn(move || {
            serve(&theirs, &keeper, &config, Some(&provider), FETCH_DEADLINE)
        });
        let authority = Authority::over(broker).unwrap();
        assert_eq!(
            authority.search("rust"),
            Ok(vec![hit("https://docs.example/rust")])
        );
        drop(authority);
        assert!(serving.join().unwrap().is_ok(), "ends when the broker goes");
        // Refused: the reason reaches the broker; a broken one is lost.
        let (keeper, _) = keeper_answering(None);
        let (broker, theirs) = UnixStream::pair().unwrap();
        let config = searching();
        let provider: Provider = Arc::new(|_, _| Ok(Vec::new()));
        std::thread::spawn(move || {
            serve(&theirs, &keeper, &config, Some(&provider), FETCH_DEADLINE)
        });
        let authority = Authority::over(broker).unwrap();
        assert!(
            matches!(authority.search("q"), Err(reason) if reason.starts_with("no search key"))
        );
        // An answer out of step loses the authority: nothing more is sent.
        let (broker, theirs) = UnixStream::pair().unwrap();
        let stand_in = std::thread::spawn(move || {
            let _ = read_frame(&theirs);
            write_frame(&theirs, b"not an answer").unwrap();
            read_frame_or_eof(&theirs).unwrap()
        });
        let authority = Authority::over(broker).unwrap();
        let unavailable = Err(String::from("the egress authority is unavailable"));
        assert_eq!(authority.search("q"), unavailable);
        assert_eq!(authority.search("q"), unavailable, "lost for good");
        drop(authority);
        assert!(
            stand_in.join().unwrap().is_none(),
            "a request was sent after it was lost"
        );
        // A request that doesn't parse ends the channel.
        let (keeper, _) = keeper_answering(None);
        let (broker, theirs) = UnixStream::pair().unwrap();
        write_frame(&broker, b"{\"request\":\"steal\"}").unwrap();
        assert!(serve(&theirs, &keeper, &searching(), None, FETCH_DEADLINE).is_err());
        // An answer naming a bad url is refused, not trusted.
        let (broker, theirs) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let _ = read_frame(&theirs);
            let bad = serde_json::json!({"reply": "results", "hits": [{"title": "x", "url": "http://plain.example/", "snippet": "y"}]});
            write_frame(&theirs, &serde_json::to_vec(&bad).unwrap()).unwrap();
        });
        let authority = Authority::over(broker).unwrap();
        assert_eq!(
            authority.search("q"),
            Err(String::from("the egress authority answered a bad result"))
        );
    }

    #[test]
    fn the_authority_runs_only_as_the_broker_starts_it() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", egress_authority_main()), failure);
    }
}
