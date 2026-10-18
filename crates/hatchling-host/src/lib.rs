//! Hatchling's frontend host: the UI's way to the broker.
//!
//! A browser can't open a Unix socket, so the host serves the built UI on
//! `127.0.0.1` and carries the browser's WebSocket to the broker's UI
//! socket (`HATCHLING_UI_SOCKET`), frame for frame. It is a transport
//! and nothing more: it decides nothing about what a message may do, and
//! the `arch` check holds it to that (its dependencies, and no
//! authorization vocabulary in its code). Every hello it carries must
//! hold the UI token, which the broker checks; the host only prints the
//! address to open, with the token in the URL's fragment, which a
//! browser never sends to a server.
//!
//! - [`http`]: the request head, the UI's files, and the upgrade.
//! - [`websocket`]: the frames, read strictly.
//! - [`bridge`]: one browser to one broker connection.

pub mod bridge;
pub mod cli;
pub mod environment;
pub mod http;
pub mod websocket;

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The port the host serves on unless told otherwise.
pub const DEFAULT_PORT: u16 = 7340;

/// The most connections served at once.
pub const MAX_CONNECTIONS: usize = 16;

/// How long a new connection may take to send its request's head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A read or write failed.
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The browser sent a frame the host doesn't accept.
    #[error("refused a frame: {0}")]
    Frame(&'static str),
    /// The browser sent a request the host doesn't answer.
    #[error("refused a request: {0}")]
    Request(&'static str),
    /// A message couldn't be framed to the broker.
    #[error("the broker's framing failed: {0}")]
    Broker(#[from] hatchling_core::Error),
    /// A required setting is missing.
    #[error("missing required configuration {0}; set it in the environment")]
    Missing(&'static str),
    /// A setting is invalid.
    #[error("invalid configuration {key}: {reason}")]
    Invalid {
        /// The setting.
        key: &'static str,
        /// Why.
        reason: String,
    },
}

/// The host's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The broker's UI socket.
    pub ui_socket: PathBuf,
    /// The built frontend.
    pub ui_dir: PathBuf,
    /// The port on `127.0.0.1`.
    pub port: u16,
    /// The broker's operator socket, when the Machine view is served too.
    pub operator_socket: Option<PathBuf>,
}

impl Settings {
    /// The settings `get` reads.
    ///
    /// # Errors
    ///
    /// [`Error::Missing`] for a required setting, and [`Error::Invalid`]
    /// for a relative path or a port that isn't one.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let absolute_path = |key: &'static str, raw: &str| -> Result<PathBuf, Error> {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(Error::Invalid {
                    key,
                    reason: format!("must be an absolute path, not {raw:?}"),
                })
            }
        };
        let absolute = |key: &'static str| -> Result<PathBuf, Error> {
            let raw = get(key).ok_or(Error::Missing(key))?;
            absolute_path(key, &raw)
        };
        let port = match get(environment::HATCHLING_HOST_PORT.name) {
            None => DEFAULT_PORT,
            Some(raw) => raw.trim().parse().map_err(|_| Error::Invalid {
                key: "HATCHLING_HOST_PORT",
                reason: format!("must be a port number, not {raw:?}"),
            })?,
        };
        let operator_socket = get(environment::HATCHLING_OPERATOR_SOCKET.name)
            .map(|raw| absolute_path("HATCHLING_OPERATOR_SOCKET", &raw))
            .transpose()?;
        Ok(Self {
            ui_socket: absolute(environment::HATCHLING_UI_SOCKET.name)?,
            ui_dir: absolute(environment::HATCHLING_HOST_UI.name)?,
            port,
            operator_socket,
        })
    }
}

/// Where the broker keeps the token for `socket` (the UI's or the
/// operator's): the socket's path with `.token` added.
#[must_use]
pub fn token_path(socket: &Path) -> PathBuf {
    let mut path = socket.as_os_str().to_owned();
    path.push(".token");
    PathBuf::from(path)
}

/// The token the broker minted for `socket`, the setting `key` names, to
/// put in the address the person opens.
///
/// # Errors
///
/// [`Error::Invalid`] when it can't be read: the broker isn't running
/// with that socket, or runs as another user.
pub fn read_token(socket: &Path, key: &'static str) -> Result<String, Error> {
    let path = token_path(socket);
    std::fs::read_to_string(&path)
        .map(|token| token.trim().to_owned())
        .map_err(|error| Error::Invalid {
            key,
            reason: format!(
                "the token {} can't be read ({error}): is the broker running with this socket, as this user?",
                path.display()
            ),
        })
}

/// The address to open: the UI on `port`, with `token` in the fragment as
/// `#hatchling-ui=<token>`, which the UI takes and clears before its own
/// routing reads the fragment.
#[must_use]
pub fn address(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/#hatchling-ui={token}")
}

/// The Machine view's address on `port`, with the operator's `token` in
/// the fragment as `#hatchling-operator=<token>`.
#[must_use]
pub fn machine_address(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/machine.html#hatchling-operator={token}")
}

/// Binds the host's listener on `127.0.0.1` only.
///
/// # Errors
///
/// [`Error::Io`] when the port can't be bound.
pub fn bind(port: u16) -> Result<TcpListener, Error> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, port))?)
}

/// Answers one connection: the UI's files, or the upgrade and its bridge.
///
/// # Errors
///
/// As [`http::read_head`], [`http::answer`] and [`bridge::bridge`].
pub fn handle(mut stream: TcpStream, settings: &Settings) -> Result<(), Error> {
    stream.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let head = http::read_head(&mut stream)?;
    // The port the request arrived on: the configured one may be 0.
    let port = stream.local_addr()?.port();
    if !head.addressed_to(port) {
        return http::forbid(&mut stream);
    }
    let socket = match head.websocket() {
        Some((http::Channel::Ui, key)) => Some((&settings.ui_socket, key)),
        Some((http::Channel::Operator, key)) => settings
            .operator_socket
            .as_ref()
            .map(|socket| (socket, key)),
        None => None,
    };
    match socket {
        Some(_) if !head.from_page_on(port) => http::forbid(&mut stream),
        Some((socket, key)) => {
            http::accept_upgrade(&mut stream, key)?;
            stream.set_read_timeout(None)?;
            bridge::bridge(stream, socket)
        }
        None => http::answer(&mut stream, &settings.ui_dir, &head),
    }
}

/// Serves `listener` forever, each connection on its own thread, at most
/// [`MAX_CONNECTIONS`] at once; one past that is closed at once.
pub fn serve(listener: &TcpListener, settings: &Settings) -> ! {
    let open = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        loop {
            let Ok((stream, _)) = listener.accept() else {
                continue;
            };
            if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                open.fetch_sub(1, Ordering::SeqCst);
                drop(stream);
                continue;
            }
            let open = &open;
            scope.spawn(move || {
                if let Err(error) = handle(stream, settings) {
                    tracing::debug!(%error, "connection ended");
                }
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixListener;

    use hatchling_core::framing::{read_frame, write_frame};

    use super::*;

    fn parse(vars: &[(&str, &str)]) -> Result<Settings, Error> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Settings::parse(|key| vars.get(key).cloned())
    }

    #[test]
    fn settings_need_both_paths_absolute_and_a_real_port() {
        let both = [
            ("HATCHLING_UI_SOCKET", "/run/hatchling/ui.sock"),
            ("HATCHLING_HOST_UI", "/srv/hatchling/ui"),
        ];
        let settings = parse(&both).unwrap();
        assert_eq!(settings.port, DEFAULT_PORT);
        assert_eq!(settings.operator_socket, None);
        let mut operated = both.to_vec();
        operated.push(("HATCHLING_OPERATOR_SOCKET", "/run/hatchling/operator.sock"));
        assert_eq!(
            parse(&operated).unwrap().operator_socket,
            Some(PathBuf::from("/run/hatchling/operator.sock"))
        );
        let mut relative = both.to_vec();
        relative.push(("HATCHLING_OPERATOR_SOCKET", "operator.sock"));
        assert!(matches!(
            parse(&relative),
            Err(Error::Invalid {
                key: "HATCHLING_OPERATOR_SOCKET",
                ..
            })
        ));
        assert_eq!(settings.ui_socket, PathBuf::from("/run/hatchling/ui.sock"));
        assert_eq!(settings.ui_dir, PathBuf::from("/srv/hatchling/ui"));
        let mut chosen = both.to_vec();
        chosen.push(("HATCHLING_HOST_PORT", " 0 "));
        assert_eq!(parse(&chosen).unwrap().port, 0);
        let mut bad = both.to_vec();
        bad.push(("HATCHLING_HOST_PORT", "70000"));
        assert!(matches!(
            parse(&bad),
            Err(Error::Invalid {
                key: "HATCHLING_HOST_PORT",
                ..
            })
        ));
        assert!(matches!(
            parse(&[("HATCHLING_HOST_UI", "/srv/ui")]),
            Err(Error::Missing("HATCHLING_UI_SOCKET"))
        ));
        assert!(matches!(
            parse(&[("HATCHLING_UI_SOCKET", "/run/ui.sock")]),
            Err(Error::Missing("HATCHLING_HOST_UI"))
        ));
        assert!(matches!(
            parse(&[
                ("HATCHLING_UI_SOCKET", "ui.sock"),
                ("HATCHLING_HOST_UI", "/srv/ui")
            ]),
            Err(Error::Invalid {
                key: "HATCHLING_UI_SOCKET",
                ..
            })
        ));
        assert!(matches!(
            parse(&[
                ("HATCHLING_UI_SOCKET", "/run/ui.sock"),
                ("HATCHLING_HOST_UI", "dist")
            ]),
            Err(Error::Invalid {
                key: "HATCHLING_HOST_UI",
                ..
            })
        ));
    }

    #[test]
    fn the_token_is_read_beside_the_socket_and_goes_only_in_the_fragment() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ui.sock");
        assert_eq!(token_path(&socket), dir.join("ui.sock.token"));
        assert!(matches!(
            read_token(&socket, "HATCHLING_UI_SOCKET"),
            Err(Error::Invalid {
                key: "HATCHLING_UI_SOCKET",
                ..
            })
        ));
        assert!(matches!(
            read_token(&socket, "HATCHLING_OPERATOR_SOCKET"),
            Err(Error::Invalid {
                key: "HATCHLING_OPERATOR_SOCKET",
                ..
            })
        ));
        std::fs::write(token_path(&socket), "abc123\n").unwrap();
        assert_eq!(
            read_token(&socket, "HATCHLING_UI_SOCKET").unwrap(),
            "abc123"
        );
        assert_eq!(
            address(7340, "abc123"),
            "http://127.0.0.1:7340/#hatchling-ui=abc123"
        );
        assert_eq!(
            machine_address(7340, "abc123"),
            "http://127.0.0.1:7340/machine.html#hatchling-operator=abc123"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_host_binds_loopback_only() {
        let listener = bind(0).unwrap();
        let local = listener.local_addr().unwrap();
        assert!(local.ip().is_loopback(), "{local}");
        assert_ne!(local.port(), 0);
    }

    /// A browser's side of the upgrade, on `stream`.
    fn upgrade(stream: &mut TcpStream) {
        upgrade_on(stream, "/ui");
    }

    /// A browser's upgrade to `path`, from a page the host on `port`
    /// served, as its own page sends it.
    fn upgrade_request(path: &str, port: u16) -> String {
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
    }

    /// The opening handshake for `path`, read to its end.
    fn upgrade_on(stream: &mut TcpStream, path: &str) {
        let port = stream.peer_addr().unwrap().port();
        stream
            .write_all(upgrade_request(path, port).as_bytes())
            .unwrap();
        let mut answer = Vec::new();
        let mut byte = [0_u8; 1];
        while !answer.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            answer.extend_from_slice(&byte);
        }
        assert!(answer.starts_with(b"HTTP/1.1 101 "));
    }

    /// A masked text frame, as a browser sends one.
    fn masked(text: &[u8]) -> Vec<u8> {
        let mask = [7_u8, 1, 9, 3];
        let mut frame = vec![0x81, 0x80 + u8::try_from(text.len()).unwrap()];
        frame.extend_from_slice(&mask);
        frame.extend(text.iter().zip(mask.iter().cycle()).map(|(b, k)| b ^ k));
        frame
    }

    #[test]
    fn a_browsers_messages_reach_the_broker_and_its_answers_come_back_unchanged() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-host-bridge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ui.sock");
        let broker = UnixListener::bind(&socket).unwrap();
        let settings = Settings {
            ui_socket: socket,
            ui_dir: dir.clone(),
            port: 0,
            operator_socket: None,
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let host = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, &settings)
            });
            let fake_broker = scope.spawn(|| {
                let (stream, _) = broker.accept().unwrap();
                let heard = read_frame(&stream).unwrap();
                write_frame(&stream, b"{\"type\":\"welcome\"}").unwrap();
                heard
            });
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            upgrade(&mut browser);
            browser.write_all(&masked(b"{\"type\":\"hello\"}")).unwrap();
            assert_eq!(websocket_text(&mut browser), b"{\"type\":\"welcome\"}");
            assert_eq!(fake_broker.join().unwrap(), b"{\"type\":\"hello\"}");
            // The broker gone: the browser is told it's closing.
            let mut close = [0_u8; 2];
            browser.read_exact(&mut close).unwrap();
            assert_eq!(close, [0x88, 0]);
            drop(browser);
            let _ = host.join().unwrap();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ping_is_answered_and_a_browser_that_closes_ends_the_brokers_side() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-close-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ui.sock");
        let broker = UnixListener::bind(&socket).unwrap();
        let settings = Settings {
            ui_socket: socket,
            ui_dir: dir.clone(),
            port: 0,
            operator_socket: None,
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let host = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, &settings)
            });
            let fake_broker = scope.spawn(|| {
                let (stream, _) = broker.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                hatchling_core::framing::read_frame_or_eof(&stream).unwrap()
            });
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            upgrade(&mut browser);
            // A masked ping with "hi": answered with a pong holding it.
            browser
                .write_all(&[0x89, 0x82, 0, 0, 0, 0, b'h', b'i'])
                .unwrap();
            let mut pong = [0_u8; 4];
            browser.read_exact(&mut pong).unwrap();
            assert_eq!(pong, [0x8A, 2, b'h', b'i']);
            // A masked close: the broker's side sees its connection end.
            browser.write_all(&[0x88, 0x80, 0, 0, 0, 0]).unwrap();
            assert_eq!(fake_broker.join().unwrap(), None);
            assert!(host.join().unwrap().is_ok());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_machine_views_channel_goes_to_the_operator_socket_only_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-op-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ui = UnixListener::bind(dir.join("ui.sock")).unwrap();
        let operator = UnixListener::bind(dir.join("operator.sock")).unwrap();
        let with = Settings {
            ui_socket: dir.join("ui.sock"),
            ui_dir: dir.clone(),
            port: 0,
            operator_socket: Some(dir.join("operator.sock")),
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let host = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, &with)
            });
            // It waits at most five seconds: a channel sent elsewhere fails
            // the test instead of hanging it.
            let fake_operator = scope.spawn(|| {
                operator.set_nonblocking(true).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let stream = loop {
                    match operator.accept() {
                        Ok((stream, _)) => break Some(stream),
                        Err(_) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break None,
                    }
                }?;
                stream.set_nonblocking(false).unwrap();
                let heard = read_frame(&stream).unwrap();
                write_frame(&stream, b"{\"type\":\"welcome\"}").unwrap();
                Some(heard)
            });
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            upgrade_on(&mut browser, "/operator");
            browser.write_all(&masked(b"{\"type\":\"hello\"}")).unwrap();
            assert_eq!(
                fake_operator.join().unwrap().as_deref(),
                Some(&b"{\"type\":\"hello\"}"[..]),
                "the operator socket heard the hello"
            );
            assert_eq!(websocket_text(&mut browser), b"{\"type\":\"welcome\"}");
            drop(browser);
            let _ = host.join().unwrap();
        });
        // Nothing reached the UI's socket.
        ui.set_nonblocking(true).unwrap();
        assert!(ui.accept().is_err());
        // Without an operator socket, /operator is no channel: a plain 404.
        let without = Settings {
            operator_socket: None,
            ..with
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let host = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, &without)
            });
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .write_all(upgrade_request("/operator", port).as_bytes())
                .unwrap();
            let mut answer = String::new();
            browser.read_to_string(&mut answer).unwrap();
            assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
            assert!(host.join().unwrap().is_ok());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_that_isnt_an_upgrade_gets_the_uis_files() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-get-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"ui").unwrap();
        let settings = Settings {
            ui_socket: dir.join("absent.sock"),
            ui_dir: dir.clone(),
            port: 0,
            operator_socket: None,
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let host = scope.spawn(|| {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, &settings)
            });
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .write_all(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
                .unwrap();
            let mut answer = String::new();
            browser.read_to_string(&mut answer).unwrap();
            assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
            assert!(answer.ends_with("\r\n\r\nui"));
            assert!(host.join().unwrap().is_ok());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_from_another_site_is_refused_and_reaches_no_socket() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-site-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"ui").unwrap();
        let ui = UnixListener::bind(dir.join("ui.sock")).unwrap();
        let operator = UnixListener::bind(dir.join("operator.sock")).unwrap();
        let settings = Settings {
            ui_socket: dir.join("ui.sock"),
            ui_dir: dir.clone(),
            port: 0,
            operator_socket: Some(dir.join("operator.sock")),
        };
        let upgrade = |path: &str, host: &str, origin: Option<&str>| {
            let origin = origin.map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
            format!(
                "GET {path} HTTP/1.1\r\nHost: {host}\r\n{origin}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
            )
        };
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let here = format!("127.0.0.1:{port}");
        let page = format!("http://127.0.0.1:{port}");
        let rebound = format!("evil.example:{port}");
        let refused = [
            // Another site's page, in the user's browser.
            upgrade("/ui", &here, Some("http://evil.example")),
            upgrade("/operator", &here, Some("http://evil.example")),
            upgrade("/ui", &here, Some("null")),
            // Not from a page at all: a browser always sends an Origin.
            upgrade("/ui", &here, None),
            // A page that rebinds its own name to 127.0.0.1.
            upgrade("/ui", &rebound, Some(&format!("http://{rebound}"))),
            format!("GET / HTTP/1.1\r\nHost: {rebound}\r\n\r\n"),
            String::from("GET / HTTP/1.1\r\n\r\n"),
            // The right page, but addressed to another port's host.
            upgrade(
                "/ui",
                &format!("127.0.0.1:{}", port.wrapping_add(1)),
                Some(&page),
            ),
        ];
        // The host on a thread of its own, joined only once every answer
        // was a refusal: a request wrongly let through holds its bridge
        // open, and the test must fail at once, not wait on it.
        let host = {
            let settings = settings.clone();
            let count = refused.len();
            std::thread::spawn(move || {
                for _ in 0..count {
                    let (stream, _) = listener.accept().unwrap();
                    handle(stream, &settings).unwrap();
                }
            })
        };
        for request in &refused {
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            browser
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            browser.write_all(request.as_bytes()).unwrap();
            let mut answer = String::new();
            let read = browser.read_to_string(&mut answer);
            assert!(
                read.is_ok() && answer.starts_with("HTTP/1.1 403 Forbidden\r\n"),
                "{request:?} got {answer:?}, then {read:?}"
            );
            assert!(answer.ends_with("\r\n\r\n"), "a refusal has no body");
        }
        host.join().unwrap();
        // Nothing reached either socket.
        ui.set_nonblocking(true).unwrap();
        operator.set_nonblocking(true).unwrap();
        assert!(ui.accept().is_err() && operator.accept().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_bridge_reports_a_broker_it_cannot_reach() {
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let _browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let missing = std::env::temp_dir().join(format!(
            "hatchling-host-missing-{}.sock",
            std::process::id()
        ));
        assert!(matches!(
            bridge::bridge(accepted, &missing),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    /// One unmasked text frame from the host, under 126 bytes.
    fn websocket_text(stream: &mut TcpStream) -> Vec<u8> {
        let mut head = [0_u8; 2];
        stream.read_exact(&mut head).unwrap();
        assert_eq!(head.first(), Some(&0x81));
        let mut payload = vec![0_u8; usize::from(*head.get(1).unwrap())];
        stream.read_exact(&mut payload).unwrap();
        payload
    }
}
