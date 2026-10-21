//! The controller in a real browser cell, driving a real Chromium
//! (WO-029, part 1): open, snapshot, act, scroll and close, against pages
//! the test serves itself.
//!
//! The cell's only network is its relay, which the test holds and answers
//! as a plain HTTP proxy for one made-up host. So every request the
//! browser makes reaches the test, and the test can say what they were:
//! only that host, and nothing of the browser's own.
//!
//! Needs a cell, a delegated resource group and a Chromium
//! (`HATCHLING_TEST_CGROUP`, `HATCHLING_TEST_BROWSER`); each is skipped
//! without, unless its `HATCHLING_REQUIRE_*` makes that a failure.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hatchling_core::browser::{
    BrowserCommand, BrowserFailure, BrowserReply, Key, PageAction, PageSnapshot,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_isolation::{
    Cell as _, CellSpec, Exposure, Filter, IsolationBackend as _, Limits, Tool,
};

/// The relay port in the cell.
const PORT: u16 = 8118;

/// The made-up host the test serves.
const HOST: &str = "hatchling.test";

const START: &str = r#"<!doctype html><title>Start</title>
<h1>Start page</h1>
<p id="status">waiting</p>
<button onclick="document.getElementById('status').textContent = 'clicked'">Go</button>
<input aria-label="Search" oninput="document.getElementById('echo').textContent = 'echo: ' + this.value">
<p id="echo"></p>
<input type="password" aria-label="Password">
<a href="/next">Next page</a>"#;

const NEXT: &str = "<!doctype html><title>Next</title><p>Second page</p>";

/// Whether `name`'s requirement makes a missing prerequisite a failure.
fn required(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

/// What the test needs, or `None` to skip: the browser, a resource group.
fn prerequisites() -> Option<(PathBuf, hatchling_sys::cgroup::CellCgroup)> {
    let platform = hatchling_isolation::platform();
    let probe = platform.identity(None, 1).ok().and_then(|identity| {
        platform
            .isolate(&CellSpec {
                identity,
                program: PathBuf::from("/bin/true"),
                argv: vec![String::from(hatchling_isolation::PROGRAM)],
                env: Vec::new(),
                workdir: None,
                endpoint: None,
                connection: false,
                exposures: Vec::new(),
                tools: Vec::new(),
                relay: None,
                filter: Filter::Program,
                limits: Limits::default(),
                cgroup: None,
                scratch: None,
            })
            .ok()
            .and_then(|cell| cell.wait().ok())
    });
    if probe.is_none_or(|exit| exit.code != 0) {
        assert!(
            !required("HATCHLING_REQUIRE_CELL"),
            "HATCHLING_REQUIRE_CELL=1 but no cell runs here"
        );
        return None;
    }
    let Some(browser) = std::env::var_os("HATCHLING_TEST_BROWSER").map(PathBuf::from) else {
        assert!(
            !required("HATCHLING_REQUIRE_BROWSER"),
            "HATCHLING_REQUIRE_BROWSER=1 but HATCHLING_TEST_BROWSER isn't set"
        );
        return None;
    };
    let Some(base) = std::env::var_os("HATCHLING_TEST_CGROUP") else {
        assert!(
            !required("HATCHLING_REQUIRE_CGROUP"),
            "HATCHLING_REQUIRE_CGROUP=1 but no delegation"
        );
        return None;
    };
    let group = hatchling_sys::cgroup::CellCgroup::create(
        Path::new(&base),
        hatchling_sys::cgroup::CgroupLimits::default(),
    )
    .unwrap();
    Some((browser, group))
}

/// A browser cell running the controller.
fn spec(browser: &Path, group: &Path, scratch: &Path) -> CellSpec {
    let name = browser.file_name().unwrap().to_str().unwrap().to_owned();
    CellSpec {
        identity: hatchling_isolation::platform().identity(None, 1).unwrap(),
        program: PathBuf::from(env!("CARGO_BIN_EXE_hatchling-browser")),
        argv: vec![String::from(hatchling_isolation::PROGRAM)],
        env: vec![
            (
                String::from("HATCHLING_BROWSER"),
                format!("/browser/{name}"),
            ),
            (
                String::from("HATCHLING_BROWSER_PROXY_PORT"),
                PORT.to_string(),
            ),
            (String::from("HOME"), String::from("/scratch")),
            (String::from("TMPDIR"), String::from("/scratch")),
            (
                String::from("HATCHLING_BROWSER_FONTS"),
                String::from("/fonts"),
            ),
        ],
        workdir: None,
        endpoint: None,
        connection: true,
        exposures: vec![
            Exposure {
                source: browser.parent().unwrap().to_owned(),
                name: String::from("browser"),
                writable: false,
                label: "browser",
            },
            Exposure {
                source: scratch.to_owned(),
                name: String::from("scratch"),
                writable: true,
                label: "scratch",
            },
            // The host's fonts: the renderer lays out text with them.
            Exposure {
                source: PathBuf::from("/usr/share/fonts"),
                name: String::from("fonts"),
                writable: false,
                label: "fonts",
            },
        ],
        // The browser is also a tool, which binds the libraries it links.
        tools: vec![Tool {
            source: browser.to_owned(),
            name: String::from("browser-libs"),
        }],
        relay: Some(PORT),
        filter: Filter::Browser,
        limits: Limits {
            memory: Some(2 << 30),
            processes: Some(512),
            ..Limits::default()
        },
        cgroup: Some(group.to_owned()),
        scratch: None,
    }
}

/// Serves `listener` as an HTTP proxy for [`HOST`] until `stop`, keeping
/// each request line it's sent. Each connection has a thread of its own:
/// the browser opens connections it doesn't use at once, and one that
/// sends nothing mustn't hold up the next.
fn proxy(listener: &TcpListener, stop: &AtomicBool, seen: &Arc<Mutex<Vec<String>>>) {
    listener.set_nonblocking(true).unwrap();
    while !stop.load(Ordering::Relaxed) {
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let seen = Arc::clone(seen);
        std::thread::spawn(move || answer(stream, &seen));
    }
}

/// Answers one proxied request: [`START`], [`NEXT`], or not found.
fn answer(mut stream: std::net::TcpStream, seen: &Mutex<Vec<String>>) {
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .is_err()
    {
        return;
    }
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
        if stream.read_exact(&mut byte).is_err() {
            return;
        }
        head.extend_from_slice(&byte);
    }
    let head = String::from_utf8_lossy(&head);
    let line = head.lines().next().unwrap_or_default().to_owned();
    seen.lock().unwrap().push(line.clone());
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let (status, body) = match target.strip_prefix(&format!("http://{HOST}")) {
        Some("/") => ("200 OK", START),
        Some("/next") => ("200 OK", NEXT),
        _ => ("404 Not Found", "not here"),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// `command`, sent; its reply.
fn send(connection: &UnixStream, command: &BrowserCommand) -> BrowserReply {
    write_frame(connection, &serde_json::to_vec(command).unwrap()).unwrap();
    serde_json::from_slice(&read_frame(connection).unwrap()).unwrap()
}

/// The snapshot in `reply`, or a default one that the assertions then
/// reject, with what came back as its text.
fn page(reply: BrowserReply) -> PageSnapshot {
    match reply {
        BrowserReply::Page { snapshot } => snapshot,
        other @ (BrowserReply::Failed { .. } | BrowserReply::Closed) => PageSnapshot {
            text: format!("not a page: {other:?}"),
            ..PageSnapshot::default()
        },
    }
}

/// The id of the element `role` named `name`.
fn element(snapshot: &PageSnapshot, role: &str, name: &str) -> u32 {
    let found = snapshot
        .elements
        .iter()
        .find(|element| element.role == role && element.name == name)
        .map(|element| element.id);
    assert!(
        found.is_some(),
        "no {role} {name:?} in {:?}",
        snapshot.elements
    );
    found.unwrap()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one session's steps, in order: each acts on the page the one before left"
)]
fn the_controller_drives_a_real_browser_in_a_cell_through_the_relay_alone() {
    let Some((browser, mut group)) = prerequisites() else {
        return;
    };
    let scratch =
        std::env::temp_dir().join(format!("hatchling-browser-cell-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut cell = hatchling_isolation::platform()
        .isolate(&spec(&browser, group.path(), &scratch))
        .unwrap();
    let connection = cell.connection().unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(90)))
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let (listener, stop, seen) = (cell.relay().unwrap(), Arc::clone(&stop), Arc::clone(&seen));
        std::thread::spawn(move || proxy(&listener, &stop, &seen))
    };

    // Nothing to act on before a page is open.
    assert_eq!(
        send(&connection, &BrowserCommand::Snapshot),
        BrowserReply::Failed {
            failure: BrowserFailure::NoPage
        }
    );
    let start = page(send(
        &connection,
        &BrowserCommand::Open {
            url: format!("http://{HOST}/"),
        },
    ));
    let start_url = format!("http://{HOST}/");
    let proxied = seen.lock().unwrap().clone();
    assert_eq!(
        start.url, start_url,
        "{}; the proxy saw {proxied:?}",
        start.text
    );
    assert_eq!(start.title, "Start");
    assert!(
        start.text.contains("Start page") && start.text.contains("waiting"),
        "{}",
        start.text
    );
    let go = element(&start, "button", "Go");
    let search = element(&start, "textbox", "Search");
    let next = element(&start, "link", "Next page");
    let password = element(&start, "password", "Password");

    // A click, through the browser's own input.
    let clicked = page(send(
        &connection,
        &BrowserCommand::Act {
            element: go,
            action: PageAction::Click,
        },
    ));
    assert!(clicked.text.contains("clicked"), "{}", clicked.text);
    // Typed text, as keystrokes would arrive.
    let typed = page(send(
        &connection,
        &BrowserCommand::Act {
            element: search,
            action: PageAction::Type {
                text: String::from("hello"),
            },
        },
    ));
    assert!(typed.text.contains("echo: hello"), "{}", typed.text);
    // A password typed into the page is never read back: not in the
    // field's name, not in the text.
    let secret = "s3cret-typed-here";
    let after = page(send(
        &connection,
        &BrowserCommand::Act {
            element: password,
            action: PageAction::Type {
                text: String::from(secret),
            },
        },
    ));
    let shown = serde_json::to_string(&after).unwrap();
    assert!(!shown.contains(secret), "{shown}");
    assert!(
        after
            .elements
            .iter()
            .any(|e| e.role == "password" && e.name == "Password"),
        "{shown}"
    );
    // A key, and a choice the element doesn't offer.
    assert!(matches!(
        send(
            &connection,
            &BrowserCommand::Act {
                element: search,
                action: PageAction::Key { key: Key::Tab }
            }
        ),
        BrowserReply::Page { .. }
    ));
    assert_eq!(
        send(
            &connection,
            &BrowserCommand::Act {
                element: go,
                action: PageAction::Choose {
                    option: String::from("x")
                }
            }
        ),
        BrowserReply::Failed {
            failure: BrowserFailure::NotPossible
        }
    );
    // A link, to a new page, which is waited for.
    let second = page(send(
        &connection,
        &BrowserCommand::Act {
            element: next,
            action: PageAction::Click,
        },
    ));
    let second_url = format!("http://{HOST}/next");
    assert_eq!(second.url, second_url, "{}", second.text);
    assert!(second.text.contains("Second page"), "{}", second.text);
    // The first page's elements are gone with it.
    assert_eq!(
        send(
            &connection,
            &BrowserCommand::Act {
                element: go,
                action: PageAction::Click
            }
        ),
        BrowserReply::Failed {
            failure: BrowserFailure::UnknownElement
        }
    );
    assert!(matches!(
        send(&connection, &BrowserCommand::Scroll { down: true }),
        BrowserReply::Page { .. }
    ));
    // A command it doesn't know is answered, not obeyed.
    let unknown = serde_json::json!({"command": "evaluate", "script": "1"});
    write_frame(&connection, &serde_json::to_vec(&unknown).unwrap()).unwrap();
    let bad: BrowserReply = serde_json::from_slice(&read_frame(&connection).unwrap()).unwrap();
    assert_eq!(
        bad,
        BrowserReply::Failed {
            failure: BrowserFailure::BadCommand
        }
    );

    assert_eq!(
        send(&connection, &BrowserCommand::Close),
        BrowserReply::Closed
    );
    let exit = cell.wait().unwrap();
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    group.finish().unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(exit.code, 0);

    // Every request the browser made reached the relay, for the test's
    // host: nothing of its own (updates, sync, safe browsing), nothing
    // around the proxy.
    let seen = seen.lock().unwrap().clone();
    let (first, second, any) = (
        format!("GET http://{HOST}/ HTTP/1.1"),
        format!("GET http://{HOST}/next HTTP/1.1"),
        format!("GET http://{HOST}/"),
    );
    assert!(seen.contains(&first), "{seen:?}");
    assert!(seen.contains(&second), "{seen:?}");
    assert!(seen.iter().all(|line| line.starts_with(&any)), "{seen:?}");
}
