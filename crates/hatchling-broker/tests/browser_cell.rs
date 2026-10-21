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

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use hatchling_broker::CellIds;
use hatchling_broker::browser_cell::BrowserLauncher;
use hatchling_broker::browser_proxy::Route;
use hatchling_broker::browser_session::BrowserSettings;
use hatchling_broker::machine;
use hatchling_core::browser::{BrowserCommand, BrowserFailure, BrowserReply};
use hatchling_core::ui::operator::{CellKind, EgressVerdict};
use hatchling_skill::CodeHash;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// The site's made-up name.
const HOST: &str = "hatchling.test";

/// The site as the browser's `CONNECT` names it.
const DESTINATION: &str = "hatchling.test:443";

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

/// The settings for `browser`, pinned to what it is now, driven by
/// `controller`, with the host's fonts.
fn settings(browser: &Path, controller: &Path) -> BrowserSettings {
    let sha256 = CodeHash::of(&std::fs::read(browser).unwrap()).to_string();
    BrowserSettings::parse(|key| match key {
        "HATCHLING_BROWSER" => Some(browser.display().to_string()),
        "HATCHLING_BROWSER_SHA256" => Some(sha256.clone()),
        "HATCHLING_BROWSER_CONTROLLER" => Some(controller.display().to_string()),
        "HATCHLING_BROWSER_FONTS" => Some(String::from("/usr/share/fonts")),
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
    let mut cell = launcher.start(cell_id).unwrap();
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
        "the browser asked for nothing of its own: {asked:?}"
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
