//! A browser cell, launched as the broker launches one (WO-029, part 2):
//! through the `browser-cell` helper, its connection and relay handed to
//! the broker, its controller driving a real Chromium, and its only way
//! out the broker's proxy.
//!
//! The site is a TLS server on this host's loopback, which the proxy's
//! route gives the name `hatchling.test`, with a certificate made fresh on
//! each run that nobody the browser trusts has signed. The browser must
//! reach it through the proxy, and refuse it: the site sees the browser's
//! hello, for that name, and never a finished handshake.
//!
//! A second site, trusted through the operator's certificate authority
//! (`HATCHLING_BROWSER_CA`), loads a page whose script reaches for what a
//! strict session must never reach: an internal address, a loopback port, a
//! site the session doesn't hold, a UDP peer. Each gets nothing, and the
//! controller, asked directly past the broker's own checks, can't open plain
//! `http`, an internal address or a redirect away either.
//!
//! Needs a cell, a delegated resource group (`HATCHLING_TEST_CGROUP`), a
//! Chromium (`HATCHLING_TEST_BROWSER`) and the built controller; each is
//! skipped without, unless its `HATCHLING_REQUIRE_*` makes that a failure.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::io::{BufRead as _, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use hatchling_broker::CellIds;
use hatchling_broker::browser_cell::BrowserLauncher;
use hatchling_broker::browser_proxy::{Route, Sites};
use hatchling_broker::browser_session::BrowserSettings;
use hatchling_broker::machine;
use hatchling_core::browser::{BrowserCommand, BrowserFailure, BrowserReply};
use hatchling_core::ui::operator::{CellKind, EgressVerdict};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// The site's made-up name.
const HOST: &str = "hatchling.test";

/// The site as the browser's `CONNECT` names it.
const DESTINATION: &str = "hatchling.test:443";

/// A site the session never holds.
const UNGRANTED: &str = "elsewhere.test";

/// Where the trusted site's redirect leads: a site nothing else names, so
/// its refusal is the redirect's alone.
const REDIRECTED: &str = "redirected.test";

/// Whether `name`'s requirement makes a missing prerequisite a failure.
fn required(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

/// What the test needs, or `None` to skip: the browser, its controller
/// and the delegated group.
fn prerequisites() -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !common::guard_cell_available().unwrap() {
        return None;
    }
    let Some(browser) = std::env::var_os("HATCHLING_TEST_BROWSER").map(PathBuf::from) else {
        assert!(
            !required("HATCHLING_REQUIRE_BROWSER"),
            "HATCHLING_REQUIRE_BROWSER=1 but HATCHLING_TEST_BROWSER isn't set"
        );
        return None;
    };
    let controller = match common::browser_controller_binary() {
        Ok(controller) => controller,
        Err(missing) => {
            assert!(!required("HATCHLING_REQUIRE_BROWSER"), "{missing}");
            return None;
        }
    };
    let Some(group) = std::env::var_os("HATCHLING_TEST_CGROUP").map(PathBuf::from) else {
        assert!(
            !required("HATCHLING_REQUIRE_CGROUP"),
            "HATCHLING_REQUIRE_CGROUP=1 but no delegation"
        );
        return None;
    };
    Some((browser, controller, group))
}

/// The settings for `browser`, pinned to its bundle as it is now, driven
/// by `controller`, with the host's fonts.
fn settings(browser: &Path, controller: &Path) -> BrowserSettings {
    settings_trusting(browser, controller, None)
}

/// [`settings`], and the operator's certificate authorities in the PEM file
/// `ca`, if one is named.
fn settings_trusting(browser: &Path, controller: &Path, ca: Option<&Path>) -> BrowserSettings {
    let sha256 = hatchling_broker::browser_bundle::digest(browser.parent().unwrap()).unwrap();
    BrowserSettings::parse(|key| match key {
        "HATCHLING_BROWSER" => Some(browser.display().to_string()),
        "HATCHLING_BROWSER_SHA256" => Some(sha256.clone()),
        "HATCHLING_BROWSER_CONTROLLER" => Some(controller.display().to_string()),
        "HATCHLING_BROWSER_FONTS" => Some(String::from("/usr/share/fonts")),
        "HATCHLING_BROWSER_CA" => ca.map(|ca| ca.display().to_string()),
        _ => None,
    })
    .unwrap()
    .unwrap()
}

// The OIDs a self-signed P-256 certificate names, DER-encoded.
const OID_ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x11];

/// One DER element: `tag`, its definite length, `content`.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = content.len();
    let mut out = vec![tag];
    if length < 0x80 {
        out.push(u8::try_from(length).unwrap());
    } else {
        let digits: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        out.push(0x80 | u8::try_from(digits.len()).unwrap());
        out.extend(digits);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// A name with one common name.
fn name(common: &str) -> Vec<u8> {
    let attribute = seq(&[OID_COMMON_NAME.to_vec(), tlv(0x0c, common.as_bytes())]);
    seq(&[tlv(0x31, &attribute)])
}

/// A certificate for [`HOST`], made fresh, that signs itself: so its name
/// is right and nobody the browser trusts vouches for it. Its DER, and
/// its key's PKCS#8.
fn self_signed() -> (Vec<u8>, Vec<u8>) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let algorithm = seq(&[OID_ECDSA_SHA256.to_vec()]);
    let key_info = seq(&[
        seq(&[OID_EC_PUBLIC_KEY.to_vec(), OID_P256.to_vec()]),
        tlv(0x03, &[&[0_u8][..], key.public_key().as_ref()].concat()),
    ]);
    let validity = seq(&[tlv(0x17, b"200101000000Z"), tlv(0x18, b"21200101000000Z")]);
    let alt_name = seq(&[
        OID_SUBJECT_ALT_NAME.to_vec(),
        tlv(0x04, &seq(&[tlv(0x82, HOST.as_bytes())])),
    ]);
    let tbs = seq(&[
        tlv(0xa0, &[0x02, 0x01, 0x02]),
        tlv(0x02, &[1]),
        algorithm.clone(),
        name(HOST),
        validity,
        name(HOST),
        key_info,
        tlv(0xa3, &seq(&[alt_name])),
    ]);
    let signature = key.sign(&rng, &tbs).unwrap();
    let certificate = seq(&[
        tbs,
        algorithm,
        tlv(0x03, &[&[0_u8][..], signature.as_ref()].concat()),
    ]);
    (certificate, pkcs8.as_ref().to_vec())
}

/// What the site saw of one connection.
#[derive(Debug)]
struct Seen {
    /// The name the browser's hello asked for.
    name: Option<String>,
    /// How it ended: an error, or a finished handshake.
    ended: String,
}

/// The finished handshake's ending: the browser trusted the site.
const TRUSTED: &str = "the handshake finished";

/// The page the trusted site serves at `/`: its script reaches for what a
/// strict session must never reach, and writes what came of each.
const PROBE: &str = r#"<!doctype html><title>Probe</title>
<p id="r">waiting</p>
<script>
// Once the page has loaded: what it reaches for doesn't hold up its load.
const seen = {};
const note = (what, how) => {
  seen[what] = how;
  document.getElementById('r').textContent =
    Object.keys(seen).sort().map(key => key + ': ' + seen[key]).join('; ');
};
const reach = (what, url) => fetch(url, { mode: 'no-cors' }).then(() => note(what, 'reached'), () => note(what, 'refused'));
window.addEventListener('load', () => {
const image = new Image();
image.onload = () => note('image', 'reached');
image.onerror = () => note('image', 'refused');
image.src = 'https://elsewhere.test/pixel.png';
const script = document.createElement('script');
script.onload = () => note('script', 'reached');
script.onerror = () => note('script', 'refused');
script.src = 'https://elsewhere.test/script.js';
document.body.append(script);
reach('internal', 'https://10.0.0.1/');
reach('loopback', 'https://127.0.0.1:9/');
reach('ungranted', 'https://elsewhere.test/fetch');
const peer = new RTCPeerConnection({ iceServers: [{ urls: 'stun:203.0.113.7:3478' }] });
peer.createDataChannel('probe');
let udp = 0;
peer.onicecandidate = event => {
  if (event.candidate && / udp /i.test(event.candidate.candidate)) { udp += 1; }
  if (!event.candidate) { note('udp candidates', String(udp)); }
};
peer.createOffer().then(offer => peer.setLocalDescription(offer));
setTimeout(() => note('udp candidates', String(udp) + ' (timed out)'), 8000);
});
</script>"#;

/// A signed-in page (WO-030): its site sets a cookie, and it registers a
/// service worker, which is what lives on in a browser that keeps its
/// profile, and must not in one that keeps only its cookies.
const SIGNED_IN: &str = r#"<!doctype html><title>Signed in</title>
<p id="r">waiting</p>
<script>
navigator.serviceWorker.register('/worker.js')
  .then(() => navigator.serviceWorker.ready)
  .then(() => navigator.serviceWorker.getRegistrations())
  .then(all => { document.getElementById('r').textContent = 'workers: ' + all.length; },
        error => { document.getElementById('r').textContent = 'no worker: ' + error; });
</script>"#;

/// A service worker that does nothing but be there.
const WORKER: &str = "self.addEventListener('fetch', () => {});";

/// What a later browser finds of the session before it: its cookies, and
/// any service worker.
const LATER: &str = r#"<!doctype html><title>Later</title>
<p id="r">waiting</p>
<script>
navigator.serviceWorker.getRegistrations()
  .then(all => { document.getElementById('r').textContent = 'workers: ' + all.length + '; cookies: ' + document.cookie; });
</script>"#;

/// The trusted site, on 127.0.0.1 with `certificate` and its `key`: it
/// serves [`PROBE`] at `/`, a redirect to an ungranted site at `/away`, and
/// nothing else, and records each request's first line in `requests`, until
/// `stop`.
fn trusted_site(
    certificate: Vec<u8>,
    key: Vec<u8>,
    stop: &Arc<AtomicBool>,
    requests: &Arc<Mutex<Vec<String>>>,
) -> (SocketAddr, JoinHandle<()>) {
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop, requests) = (Arc::clone(stop), Arc::clone(requests));
    let serving = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((tcp, _)) => {
                    let (config, requests) = (Arc::clone(&config), Arc::clone(&requests));
                    std::thread::spawn(move || serve(&config, tcp, &requests));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    (addr, serving)
}

/// One connection to the trusted site: its request answered.
fn serve(config: &Arc<rustls::ServerConfig>, mut tcp: TcpStream, requests: &Mutex<Vec<String>>) {
    if tcp.set_nonblocking(false).is_err()
        || tcp.set_read_timeout(Some(Duration::from_secs(10))).is_err()
    {
        return;
    }
    let Ok(mut tls) = rustls::ServerConnection::new(Arc::clone(config)) else {
        return;
    };
    let mut stream = rustls::Stream::new(&mut tls, &mut tcp);
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
        header.clear();
    }
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    requests.lock().unwrap().push(line.trim_end().to_owned());
    let response = match path.as_str() {
        "/" => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PROBE}",
            PROBE.len()
        ),
        "/away" => format!(
            "HTTP/1.1 302 Found\r\nlocation: https://{REDIRECTED}/landed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        ),
        "/signed-in" => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\nset-cookie: session=from-the-site; Path=/; Secure; Max-Age=3600\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{SIGNED_IN}",
            SIGNED_IN.len()
        ),
        "/worker.js" => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/javascript\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{WORKER}",
            WORKER.len()
        ),
        "/later" => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{LATER}",
            LATER.len()
        ),
        _ => {
            String::from("HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        }
    };
    drop(reader);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    tls.send_close_notify();
    let _ = tls.complete_io(&mut tcp);
    let mut rest = Vec::new();
    let _ = tcp.read_to_end(&mut rest);
}

/// `certificate` as a PEM file in `dir`: what an operator names.
fn pem(dir: &Path, certificate: &[u8]) -> PathBuf {
    use base64::Engine as _;
    let body = base64::engine::general_purpose::STANDARD.encode(certificate);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect();
    let path = dir.join("ca.pem");
    std::fs::write(
        &path,
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        ),
    )
    .unwrap();
    path
}

/// The site, on 127.0.0.1 with a self-signed certificate, recording each
/// connection in `seen` until `stop`.
fn site(stop: &Arc<AtomicBool>, seen: &Arc<Mutex<Vec<Seen>>>) -> (SocketAddr, JoinHandle<()>) {
    let (certificate, key) = self_signed();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop, seen) = (Arc::clone(stop), Arc::clone(seen));
    let serving = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((tcp, _)) => {
                    let one = handshake(&config, tcp);
                    seen.lock().unwrap().push(one);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    (addr, serving)
}

/// One connection's handshake, as far as the browser takes it.
fn handshake(config: &Arc<rustls::ServerConfig>, mut tcp: TcpStream) -> Seen {
    tcp.set_nonblocking(false).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut tls = rustls::ServerConnection::new(Arc::clone(config)).unwrap();
    let ended = loop {
        match tls.complete_io(&mut tcp) {
            Err(error) => break error.to_string(),
            Ok(_) if !tls.is_handshaking() => break String::from(TRUSTED),
            Ok((0, 0)) => break String::from("closed mid-handshake"),
            Ok(_) => {}
        }
    };
    Seen {
        name: tls.server_name().map(str::to_owned),
        ended,
    }
}

#[test]
fn a_browser_cell_reaches_a_site_only_through_the_broker_and_refuses_a_bad_certificate() {
    let Some((browser, controller, group)) = prerequisites() else {
        return;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (addr, serving) = site(&stop, &seen);
    // The route names one site, and records every name it's asked for.
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let route = {
        let asked = Arc::clone(&asked);
        Route::with(
            move |destination| {
                asked.lock().unwrap().push(destination.to_owned());
                if destination == DESTINATION {
                    Ok(vec![addr])
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                }
            },
            |_| false,
        )
    };
    let launcher = BrowserLauncher::new(
        common::spawner(),
        settings(&browser, &controller),
        CellIds::default(),
        group,
        route,
        Duration::from_secs(300),
    );
    let cell_id = "browser-launch";
    let sites = Sites::default();
    sites.grant(DESTINATION);
    let mut cell = launcher.start(cell_id, sites.clone(), None).unwrap();
    assert!(
        machine::cells()
            .iter()
            .any(|cell| cell.cell_id == cell_id && cell.kind == CellKind::Browser),
        "listed while it runs"
    );

    let reply = cell
        .command(&BrowserCommand::Open {
            url: format!("https://{HOST}/"),
        })
        .unwrap();
    assert_eq!(
        reply,
        BrowserReply::Failed {
            failure: BrowserFailure::LoadFailed
        }
    );
    // A site the session doesn't hold: refused before its name is looked
    // up, and kept for the owner to see.
    let reply = cell
        .command(&BrowserCommand::Open {
            url: format!("https://{UNGRANTED}/"),
        })
        .unwrap();
    assert_eq!(
        reply,
        BrowserReply::Failed {
            failure: BrowserFailure::LoadFailed
        }
    );
    let ungranted = format!("{UNGRANTED}:443");
    assert_eq!(sites.refused(), [ungranted]);
    let count = cell.finish();
    stop.store(true, Ordering::SeqCst);
    serving.join().unwrap();
    assert!(
        !machine::cells().iter().any(|cell| cell.cell_id == cell_id),
        "gone once it's finished"
    );

    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let _ = writeln!(std::io::stdout(), "the site saw: {seen:?}");
    assert!(
        !seen.is_empty(),
        "the browser reached the site, through the proxy"
    );
    for one in &seen {
        assert_eq!(one.name.as_deref(), Some(HOST), "{one:?}");
        assert_ne!(
            one.ended, TRUSTED,
            "the browser trusted a certificate nobody it trusts signed"
        );
    }
    assert!(count.tunnelled >= 1, "{count:?}");
    let asked = std::mem::take(&mut *asked.lock().unwrap());
    assert!(
        asked.iter().all(|destination| destination == DESTINATION),
        "nothing else was looked up, the ungranted site included: {asked:?}"
    );
    assert!(
        machine::decisions()
            .iter()
            .any(|decision| decision.cell_id == cell_id
                && decision.destination == DESTINATION
                && decision.verdict == EgressVerdict::Allowed),
        "the tunnel is in the egress log"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one session's steps, in order, then what each side saw of them"
)]
fn a_page_in_a_real_browser_reaches_only_the_sites_its_session_holds() {
    let Some((browser, controller, group)) = prerequisites() else {
        return;
    };
    // The site's key is the operator's authority: the browser trusts it.
    let (certificate, key) = self_signed();
    let scratch =
        std::env::temp_dir().join(format!("hatchling-browser-reach-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let ca = pem(&scratch, &certificate);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (addr, serving) = trusted_site(certificate, key, &stop, &requests);
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let route = {
        let asked = Arc::clone(&asked);
        Route::with(
            move |destination| {
                asked.lock().unwrap().push(destination.to_owned());
                if destination == DESTINATION {
                    Ok(vec![addr])
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                }
            },
            |_| false,
        )
    };
    let launcher = BrowserLauncher::new(
        common::spawner(),
        settings_trusting(&browser, &controller, Some(&ca)),
        CellIds::default(),
        group,
        route,
        Duration::from_secs(300),
    );
    let cell_id = "browser-reach";
    let sites = Sites::default();
    sites.grant(DESTINATION);
    let mut cell = launcher.start(cell_id, sites.clone(), None).unwrap();
    let page = |reply: BrowserReply| match reply {
        BrowserReply::Page { snapshot } => snapshot,
        other @ (BrowserReply::Failed { .. }
        | BrowserReply::Closed
        | BrowserReply::Frame { .. }
        | BrowserReply::Cookies { .. }
        | BrowserReply::CookiesSet) => hatchling_core::browser::PageSnapshot {
            text: format!("not a page: {other:?}"),
            ..hatchling_core::browser::PageSnapshot::default()
        },
    };

    // The granted site loads: the operator's authority vouches for it.
    let start = page(
        cell.command(&BrowserCommand::Open {
            url: format!("https://{HOST}/"),
        })
        .unwrap(),
    );
    if start.title != "Probe" {
        let _ = writeln!(
            std::io::stdout(),
            "the page didn't load; the site served {:?}, the proxy refused {:?}",
            requests.lock().unwrap(),
            sites.refused()
        );
    }
    assert_eq!(start.title, "Probe", "{}", start.text);
    // Its script's reaching ends: each refused, and no UDP candidate.
    let mut shown = String::new();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(250));
        shown = page(cell.command(&BrowserCommand::Snapshot).unwrap()).text;
        if shown.contains("udp candidates") && shown.matches("refused").count() >= 5 {
            break;
        }
    }
    for wanted in [
        "image: refused",
        "script: refused",
        "internal: refused",
        "loopback: refused",
        "ungranted: refused",
        "udp candidates: 0",
    ] {
        assert!(shown.contains(wanted), "{wanted} in {shown}");
    }
    // Asked directly, past the broker's own checks on an agent's op: the
    // proxy refuses plain http, an internal address and a redirect away.
    for url in [
        format!("http://{HOST}/"),
        String::from("https://10.0.0.1/"),
        format!("https://127.0.0.1:{}/", addr.port()),
    ] {
        let reply = cell
            .command(&BrowserCommand::Open { url: url.clone() })
            .unwrap();
        let loaded = matches!(&reply, BrowserReply::Page { snapshot } if snapshot.title == "Probe");
        assert!(!loaded, "{url} loaded: {reply:?}");
    }
    let away = cell
        .command(&BrowserCommand::Open {
            url: format!("https://{HOST}/away"),
        })
        .unwrap();
    // Where the redirect led is the browser's own error page: the site it
    // names is refused, and never looked up (below).
    let _ = writeln!(std::io::stdout(), "the redirect away: {away:?}");
    let count = cell.finish();
    stop.store(true, Ordering::SeqCst);
    serving.join().unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();

    // What each side saw.
    let requests = std::mem::take(&mut *requests.lock().unwrap());
    let _ = writeln!(std::io::stdout(), "the site served: {requests:?}");
    assert!(
        requests.iter().any(|line| line.starts_with("GET / ")),
        "{requests:?}"
    );
    assert!(
        requests.iter().any(|line| line.starts_with("GET /away ")),
        "{requests:?}"
    );
    assert!(
        requests.iter().all(|line| line.starts_with("GET / ")
            || line.starts_with("GET /away ")
            || line.starts_with("GET /favicon.ico ")),
        "the site saw only what was asked of it: {requests:?}"
    );
    let asked = std::mem::take(&mut *asked.lock().unwrap());
    assert!(
        asked.iter().all(|destination| destination == DESTINATION),
        "no other name was looked up: {asked:?}"
    );
    let refused = sites.refused();
    assert!(
        refused.contains(&String::from("elsewhere.test:443")),
        "{refused:?}"
    );
    // The redirect's site, which only the redirect names: refused by the
    // proxy, never looked up (above), and never shown.
    assert!(
        refused.contains(&format!("{REDIRECTED}:443")),
        "the redirect wasn't refused: {refused:?}"
    );
    let landed = matches!(&away, BrowserReply::Page { snapshot } if snapshot.url.contains(REDIRECTED) && !snapshot.text.is_empty());
    assert!(!landed, "the redirect landed: {away:?}");
    assert!(
        !refused
            .iter()
            .any(|site| site.starts_with("10.") || site.starts_with("127.")),
        "an address is refused before it's a site the owner could be asked for: {refused:?}"
    );
    assert!(count.refused >= 3, "{count:?}");
    assert!(
        machine::decisions()
            .iter()
            .filter(|decision| decision.cell_id == cell_id
                && decision.verdict == EgressVerdict::Allowed)
            .all(|decision| decision.destination == DESTINATION),
        "only the granted site was let through"
    );
}

/// The text of a page `cell` shows once it says `ready` (or after about
/// ten seconds, whatever it says then).
fn waited_for(cell: &mut hatchling_broker::browser_cell::BrowserCell, ready: &str) -> String {
    let mut shown = String::new();
    for _ in 0..40 {
        if let BrowserReply::Page { snapshot } = cell.command(&BrowserCommand::Snapshot).unwrap() {
            shown = snapshot.text;
        }
        if shown.contains(ready) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    shown
}

#[test]
fn a_kept_session_is_its_cookies_alone_and_a_service_worker_dies_with_its_browser() {
    let Some((browser, controller, group)) = prerequisites() else {
        return;
    };
    let (certificate, key) = self_signed();
    let scratch =
        std::env::temp_dir().join(format!("hatchling-browser-kept-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let ca = pem(&scratch, &certificate);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (addr, serving) = trusted_site(certificate, key, &stop, &requests);
    let route = Route::with(
        move |destination| {
            if destination == DESTINATION {
                Ok(vec![addr])
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            }
        },
        |_| false,
    );
    let launcher = BrowserLauncher::new(
        common::spawner(),
        settings_trusting(&browser, &controller, Some(&ca)),
        CellIds::default(),
        group,
        route,
        Duration::from_secs(300),
    );
    let sites = Sites::default();
    sites.grant(DESTINATION);

    // The first browser signs in: the site's cookie, and a service worker.
    let mut first = launcher
        .start("browser-first", sites.clone(), None)
        .unwrap();
    first
        .command(&BrowserCommand::Open {
            url: format!("https://{HOST}/signed-in"),
        })
        .unwrap();
    let registered = waited_for(&mut first, "workers:");
    assert!(registered.contains("workers: 1"), "{registered}");
    let BrowserReply::Cookies { cookies, next } =
        first.command(&BrowserCommand::Cookies { from: 0 }).unwrap()
    else {
        unreachable!("a jar");
    };
    assert_eq!(next, None);
    assert!(
        cookies
            .iter()
            .any(|cookie| cookie.name == "session" && cookie.value == "from-the-site"),
        "the jar is read out"
    );
    // The browser ends, as a session's does, and its profile with it.
    let _first = first.finish();

    // The next starts empty, and is given the jar alone.
    let mut later = launcher.start("browser-later", sites, None).unwrap();
    assert_eq!(
        later
            .command(&BrowserCommand::SetCookies { cookies })
            .unwrap(),
        BrowserReply::CookiesSet
    );
    later
        .command(&BrowserCommand::Open {
            url: format!("https://{HOST}/later"),
        })
        .unwrap();
    let found = waited_for(&mut later, "workers:");
    let _later = later.finish();
    stop.store(true, Ordering::SeqCst);
    serving.join().unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();
    assert!(
        found.contains("workers: 0"),
        "no service worker lives on: {found}"
    );
    assert!(
        found.contains("session=from-the-site"),
        "the session does: {found}"
    );
}

/// What of a browser's profile is on this host under `dir`, written since
/// `since`: a file or directory a profile is made of, by its name, or a file
/// holding the signed-in page's service worker.
fn profile_files(dir: &Path, since: SystemTime) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        let (path, name) = (entry.path(), entry.file_name());
        let fresh = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified >= since);
        let named = ["Cookies", "Preferences", "Local State", "Service Worker"]
            .iter()
            .any(|own| name == *own);
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if fresh && named {
                found.push(path.clone());
            }
            found.extend(profile_files(&path, since));
        } else if kind.is_file() && fresh {
            let worker = || {
                std::fs::read(&path).is_ok_and(|bytes| {
                    bytes
                        .windows(WORKER.len())
                        .any(|window| window == WORKER.as_bytes())
                })
            };
            if named || worker() {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn a_browser_cell_killed_mid_session_leaves_no_profile_on_the_host() {
    let Some((browser, controller, group)) = prerequisites() else {
        return;
    };
    let since = SystemTime::now();
    let (certificate, key) = self_signed();
    let scratch =
        std::env::temp_dir().join(format!("hatchling-browser-killed-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let ca = pem(&scratch, &certificate);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (addr, serving) = trusted_site(certificate, key, &stop, &requests);
    let route = Route::with(
        move |destination| {
            if destination == DESTINATION {
                Ok(vec![addr])
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            }
        },
        |_| false,
    );
    // A group of its own, so the one cell in it is this test's.
    let mut parent = hatchling_sys::cgroup::CellCgroup::create(
        &group,
        hatchling_sys::cgroup::CgroupLimits::default(),
    )
    .unwrap();
    let launcher = BrowserLauncher::new(
        common::spawner(),
        settings_trusting(&browser, &controller, Some(&ca)),
        CellIds::default(),
        parent.path().to_path_buf(),
        route,
        Duration::from_secs(300),
    );
    let sites = Sites::default();
    sites.grant(DESTINATION);

    // Signed in: the site's cookie, and a service worker, in its profile.
    let mut cell = launcher.start("browser-killed", sites, None).unwrap();
    cell.command(&BrowserCommand::Open {
        url: format!("https://{HOST}/signed-in"),
    })
    .unwrap();
    let registered = waited_for(&mut cell, "workers:");
    assert!(registered.contains("workers: 1"), "{registered}");
    let jar = cell.command(&BrowserCommand::Cookies { from: 0 }).unwrap();
    let signed_in = matches!(&jar, BrowserReply::Cookies { cookies, .. }
        if cookies.iter().any(|cookie| cookie.value == "from-the-site"));
    assert!(signed_in, "the browser holds no session");
    let temp = std::env::temp_dir();
    assert_eq!(
        profile_files(&temp, since),
        Vec::<PathBuf>::new(),
        "its profile is on the host while it runs"
    );

    // Killed mid-session, as the kernel kills a cell: every process in its
    // group at once, with no word to the browser.
    let leaves: Vec<PathBuf> = std::fs::read_dir(parent.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    assert_eq!(leaves.len(), 1, "one cell's group: {leaves:?}");
    let leaf = leaves.first().unwrap().clone();
    std::fs::write(leaf.join("cgroup.kill"), "1").unwrap();
    let mut emptied = false;
    for _ in 0..100 {
        emptied = std::fs::read_to_string(leaf.join("cgroup.procs"))
            .is_ok_and(|procs| procs.trim().is_empty());
        if emptied {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(emptied, "the cell outlived its kill");
    let after = cell.command(&BrowserCommand::Snapshot);
    assert!(after.is_err(), "a killed cell answered: {after:?}");
    // The broker lets it go, as a session whose browser broke does.
    drop(cell);
    stop.store(true, Ordering::SeqCst);
    serving.join().unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();
    assert!(!leaf.exists(), "the cell's group outlived it");
    assert_eq!(
        profile_files(&temp, since),
        Vec::<PathBuf>::new(),
        "its profile is on the host after the kill"
    );
    parent.finish().unwrap();
}
