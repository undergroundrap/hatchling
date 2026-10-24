//! A browser session's configuration (WO-029): the operator's pinned
//! browser, the controller that drives it, and the fonts it lays out text
//! with.
//!
//! Hatchling bundles no browser. The operator names one
//! (`HATCHLING_BROWSER`) and the digest of its whole bundle, its
//! directory (`HATCHLING_BROWSER_SHA256`), and the browser runs from a
//! snapshot of the bundle checked against it as it's copied
//! ([`crate::browser_bundle`]): a bundle that isn't the pinned one is never
//! started. The controller
//! (`HATCHLING_BROWSER_CONTROLLER`) is Hatchling's own program, like the
//! agent's binary. Fonts (`HATCHLING_BROWSER_FONTS`) are a host directory
//! the cell sees read-only; without them the browser can't lay out text.
//!
//! The operator may name certificate authorities the browser trusts beside
//! its own (`HATCHLING_BROWSER_CA`, a PEM file): an internal or corporate
//! CA. The broker reads them once, at start, and the cell is given only
//! their public keys' SHA-256 ([`ca_key`]): Chromium loads a site whose
//! chain holds one of those keys whatever else its certificates say, so a
//! key named here is trusted as fully as the operator's own. The proxy's
//! checks stay as they are: it's still only the sites a session holds that
//! the browser reaches at all.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::browser_proxy::RELAY_PORT;
use crate::cell::{BrowserDirs, CellLimits};
use crate::environment as env;

/// The most certificate authorities the operator may name.
pub const MAX_BROWSER_CAS: usize = 8;

/// The largest file of them, in bytes.
pub const MAX_BROWSER_CA_BYTES: u64 = 64 * 1024;

/// A browser cell's memory, as a whole: 2 GiB.
pub const BROWSER_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

/// A browser cell's processes, threads included: a browser runs several
/// processes of many threads each.
pub const BROWSER_PROCESSES: u64 = 512;

/// A browser cell's private scratch `tmpfs`: its profile, its caches and
/// its temporary files, 512 MiB, within [`BROWSER_MEMORY`].
pub const BROWSER_SCRATCH: NonZeroU64 = match NonZeroU64::new(512 * 1024 * 1024) {
    Some(bytes) => bytes,
    None => NonZeroU64::MIN,
};

/// The libraries NSS opens when it starts, by name, not when the browser
/// is linked: its software token and the crypto that loads. Without them
/// the browser can't start TLS, and aborts.
const NSS_MODULES: [&str; 2] = ["libsoftokn3.so", "libfreeblpriv3.so"];

/// Where the host's libraries are, in the order they're looked in.
const LIBRARY_DIRS: [&str; 4] = [
    "/usr/lib/x86_64-linux-gnu",
    "/lib/x86_64-linux-gnu",
    "/usr/lib64",
    "/usr/lib",
];

/// NSS's modules as this host has them now: what a browser cell is given.
pub(crate) fn host_nss_modules() -> Vec<PathBuf> {
    nss_modules(Path::is_file)
}

/// NSS's modules, in the first of [`LIBRARY_DIRS`] that holds NSS itself
/// (`libnss3.so`), where NSS looks for them first: each one `is_file`
/// finds there.
fn nss_modules(is_file: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    LIBRARY_DIRS
        .iter()
        .map(Path::new)
        .find(|dir| is_file(&dir.join("libnss3.so")))
        .map(|dir| {
            NSS_MODULES
                .iter()
                .map(|module| dir.join(module))
                .filter(|module| is_file(module))
                .collect()
        })
        .unwrap_or_default()
}

/// The operator's browser settings: the pinned browser, the controller,
/// and the fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSettings {
    browser: PathBuf,
    sha256: String,
    controller: PathBuf,
    fonts: Option<PathBuf>,
    /// The operator's certificate authorities, by their keys' SHA-256 in
    /// standard base64.
    ca_keys: Vec<String>,
}

impl BrowserSettings {
    /// The settings `get` names: `None` when no browser is configured.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] naming the setting: a browser without its
    /// SHA-256 or a controller, a SHA-256 or another browser setting
    /// without a browser, a path that isn't absolute, or a SHA-256 that
    /// isn't 64 hex digits.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, Error> {
        let trimmed = |raw: Option<String>| raw.map(|raw| raw.trim().to_owned());
        let browser = trimmed(get(env::HATCHLING_BROWSER.name));
        let sha256 = trimmed(get(env::HATCHLING_BROWSER_SHA256.name));
        let controller = trimmed(get(env::HATCHLING_BROWSER_CONTROLLER.name));
        let fonts = trimmed(get(env::HATCHLING_BROWSER_FONTS.name));
        let ca = trimmed(get(env::HATCHLING_BROWSER_CA.name));
        let Some(browser) = browser else {
            for (key, value) in [
                (env::HATCHLING_BROWSER_SHA256.name, &sha256),
                (env::HATCHLING_BROWSER_CONTROLLER.name, &controller),
                (env::HATCHLING_BROWSER_FONTS.name, &fonts),
                (env::HATCHLING_BROWSER_CA.name, &ca),
            ] {
                if value.is_some() {
                    return Err(invalid(key, "is set, but HATCHLING_BROWSER isn't"));
                }
            }
            return Ok(None);
        };
        let browser = absolute(env::HATCHLING_BROWSER.name, &browser)?;
        if browser.file_name().and_then(|name| name.to_str()).is_none() {
            return Err(invalid(env::HATCHLING_BROWSER.name, "names no file"));
        }
        let sha256 = sha256
            .ok_or_else(|| {
                invalid(
                    env::HATCHLING_BROWSER_SHA256.name,
                    "is required with HATCHLING_BROWSER",
                )
            })?
            .to_ascii_lowercase();
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid(
                env::HATCHLING_BROWSER_SHA256.name,
                "isn't 64 hex digits",
            ));
        }
        let controller = controller.ok_or_else(|| {
            invalid(
                env::HATCHLING_BROWSER_CONTROLLER.name,
                "is required with HATCHLING_BROWSER",
            )
        })?;
        let controller = absolute(env::HATCHLING_BROWSER_CONTROLLER.name, &controller)?;
        let fonts = fonts
            .map(|fonts| absolute(env::HATCHLING_BROWSER_FONTS.name, &fonts))
            .transpose()?;
        let ca_keys = match ca {
            Some(ca) => ca_keys(&absolute(env::HATCHLING_BROWSER_CA.name, &ca)?)?,
            None => Vec::new(),
        };
        Ok(Some(Self {
            browser,
            sha256,
            controller,
            fonts,
            ca_keys,
        }))
    }

    /// The controller, the cell's program.
    #[must_use]
    pub fn controller(&self) -> &Path {
        &self.controller
    }

    /// The fonts' directory, if any.
    #[must_use]
    pub fn fonts(&self) -> Option<&Path> {
        self.fonts.as_deref()
    }

    /// The browser's bundle: its directory.
    #[must_use]
    pub fn bundle(&self) -> &Path {
        self.browser.parent().unwrap_or_else(|| Path::new("/"))
    }

    /// A verified snapshot of the browser's bundle: copied now, and kept
    /// only if its digest is the pin ([`crate::browser_bundle`]).
    ///
    /// # Errors
    ///
    /// A sentence for the operator: the bundle couldn't be copied, or
    /// isn't the pinned one, and is never started.
    pub fn snapshot(&self) -> Result<crate::browser_bundle::Snapshot, String> {
        crate::browser_bundle::Snapshot::take(self.bundle(), &self.sha256)
    }

    /// A browser cell's own part: the browser from the snapshot of its
    /// bundle at `snapshot`, the relay handed over at `handoff`, and NSS's
    /// modules as the host has them now.
    #[must_use]
    pub fn dirs(&self, snapshot: &Path, handoff: PathBuf) -> BrowserDirs {
        BrowserDirs {
            browser_dir: snapshot.to_path_buf(),
            browser_name: self
                .browser
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            fonts: self.fonts.clone(),
            nss: host_nss_modules(),
            ca_keys: self.ca_keys.clone(),
            relay: RELAY_PORT,
            handoff,
        }
    }
}

/// A browser cell's limits: [`BROWSER_MEMORY`] and [`BROWSER_PROCESSES`]
/// for the cell as a whole, and the defaults for the rest.
#[must_use]
pub fn browser_limits() -> CellLimits {
    CellLimits {
        processes: Some(BROWSER_PROCESSES),
        memory: Some(BROWSER_MEMORY),
        ..CellLimits::default()
    }
}

/// The keys of the certificate authorities in the PEM file at `path`, each
/// its public key's SHA-256 in standard base64 ([`ca_key`]).
///
/// # Errors
///
/// [`Error::InvalidConfig`] naming `HATCHLING_BROWSER_CA`: a file that
/// can't be read or is past [`MAX_BROWSER_CA_BYTES`], one with no
/// certificate or more than [`MAX_BROWSER_CAS`], or a certificate that
/// doesn't parse.
fn ca_keys(path: &Path) -> Result<Vec<String>, Error> {
    use std::io::Read as _;
    let key = env::HATCHLING_BROWSER_CA.name;
    let file = std::fs::File::open(path).map_err(|_| invalid(key, "can't be read"))?;
    let mut pem = String::new();
    file.take(MAX_BROWSER_CA_BYTES + 1)
        .read_to_string(&mut pem)
        .map_err(|_| invalid(key, "isn't readable text"))?;
    if u64::try_from(pem.len()).map_or(true, |len| len > MAX_BROWSER_CA_BYTES) {
        return Err(invalid(key, "is past 64 KiB"));
    }
    let certificates = pem_certificates(&pem)
        .ok_or_else(|| invalid(key, "holds a certificate that isn't base64"))?;
    if certificates.is_empty() {
        return Err(invalid(key, "holds no certificate"));
    }
    if certificates.len() > MAX_BROWSER_CAS {
        return Err(invalid(key, "holds more than 8 certificates"));
    }
    certificates
        .iter()
        .map(|der| {
            ca_key(der).ok_or_else(|| invalid(key, "holds a certificate that doesn't parse"))
        })
        .collect()
}

/// The DER of each `CERTIFICATE` block in `pem`, in order; `None` if one
/// isn't base64.
fn pem_certificates(pem: &str) -> Option<Vec<Vec<u8>>> {
    use base64::Engine as _;
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut certificates = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find(BEGIN) {
        let after = rest.get(start + BEGIN.len()..)?;
        let end = after.find(END)?;
        let body: String = after
            .get(..end)?
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        certificates.push(
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .ok()?,
        );
        rest = after.get(end + END.len()..)?;
    }
    Some(certificates)
}

/// A certificate's public key, as Chromium names one it's told to trust:
/// the SHA-256 of its `SubjectPublicKeyInfo`, in standard base64. `None`
/// if `der` isn't a certificate.
#[must_use]
pub fn ca_key(der: &[u8]) -> Option<String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    // Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version
    // OPTIONAL, serial, signature, issuer, validity, subject,
    // subjectPublicKeyInfo, ... }, ... }
    let (certificate, _, _) = der_element(der, 0x30)?;
    let (tbs, _, _) = der_element(certificate, 0x30)?;
    let mut fields = tbs;
    if fields.first() == Some(&0xa0) {
        fields = der_element(fields, 0xa0)?.2;
    }
    for tag in [0x02, 0x30, 0x30, 0x30, 0x30] {
        fields = der_element(fields, tag)?.2;
    }
    let (_, spki, _) = der_element(fields, 0x30)?;
    Some(base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(spki)))
}

/// The DER element at the start of `der`, if its tag is `tag`: its
/// content, the whole element, and what follows it.
fn der_element(der: &[u8], tag: u8) -> Option<(&[u8], &[u8], &[u8])> {
    let (&first, rest) = der.split_first()?;
    if first != tag {
        return None;
    }
    let (&head, rest) = rest.split_first()?;
    let (len, rest) = if head < 0x80 {
        (usize::from(head), rest)
    } else {
        let width = usize::from(head & 0x7f);
        if width == 0 || width > 3 {
            return None;
        }
        let (digits, rest) = rest.split_at_checked(width)?;
        let len = digits
            .iter()
            .fold(0usize, |len, digit| (len << 8) | usize::from(*digit));
        (len, rest)
    };
    let content = rest.get(..len)?;
    let whole = der.get(..der.len() - rest.len() + len)?;
    Some((content, whole, rest.get(len..)?))
}

/// `raw` as an absolute path, or the setting refused.
fn absolute(key: &'static str, raw: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(invalid(key, "isn't an absolute path"))
    }
}

fn invalid(key: &'static str, reason: &str) -> Error {
    Error::InvalidConfig {
        key,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn parse(vars: &[(&str, &str)]) -> Result<Option<BrowserSettings>, Error> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        BrowserSettings::parse(|key| vars.get(key).cloned())
    }

    fn refused(vars: &[(&str, &str)]) -> (&'static str, String) {
        match parse(vars) {
            Err(Error::InvalidConfig { key, reason }) => (key, reason),
            other => ("not refused", format!("{other:?}")),
        }
    }

    const SHA: &str = "20FFC88BE1C6F7DC8351BEBFACA965FF9A07617BDDF696F0B67EF85E5868B29A";

    fn full() -> Vec<(&'static str, &'static str)> {
        vec![
            ("HATCHLING_BROWSER", " /opt/chrome/chrome-headless-shell "),
            ("HATCHLING_BROWSER_SHA256", SHA),
            (
                "HATCHLING_BROWSER_CONTROLLER",
                "/usr/lib/hatchling/hatchling-browser",
            ),
            ("HATCHLING_BROWSER_FONTS", "/usr/share/fonts"),
        ]
    }

    /// Two certificate authorities, made with OpenSSL, and their keys'
    /// SHA-256 as OpenSSL computes it
    /// (`openssl x509 -pubkey | openssl pkey -pubin -outform der |
    /// openssl dgst -sha256 -binary | base64`): an oracle of its own.
    const EC_CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBmTCCAT+gAwIBAgIURYIbgJQVZyzpiJZGU0EIjyEPKh8wCgYIKoZIzj0EAwIw\nITEfMB0GA1UEAwwWSGF0Y2hsaW5nIHRlc3QgQ0EgKEVDKTAgFw0yNjEwMjIxNTE5\nNTRaGA8yMTI2MDkyODE1MTk1NFowITEfMB0GA1UEAwwWSGF0Y2hsaW5nIHRlc3Qg\nQ0EgKEVDKTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABLMAi2r9z/A9ATfK4CN6\nLQ8GhA2DU4nC5EoUc+ZUg+kLus8XYYpJDo5uVgeXOuJ1c3diPFcAUQ9N47Z2QZWP\n1byjUzBRMB0GA1UdDgQWBBSEW5BY8dSCN7f9iYjX0JpwfsBHbzAfBgNVHSMEGDAW\ngBSEW5BY8dSCN7f9iYjX0JpwfsBHbzAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49\nBAMCA0gAMEUCIBHsUmSpPlJ/Oiw1wZ+1qUUwg/wDxY5y3bD5+Mu6G4hCAiEAxAn+\nisH+nS4UctO33A5BZ13hpO8qPgHbeJeIJbjT5BI=\n-----END CERTIFICATE-----";
    const EC_CA_KEY: &str = "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYxGE=";
    const RSA_CA: &str = "-----BEGIN CERTIFICATE-----\nMIIDJzCCAg+gAwIBAgIUW3gK3yawyMqErQU1dSDNz+aq8zMwDQYJKoZIhvcNAQEL\nBQAwIjEgMB4GA1UEAwwXSGF0Y2hsaW5nIHRlc3QgQ0EgKFJTQSkwIBcNMjYxMDIy\nMTUxOTU0WhgPMjEyNjA5MjgxNTE5NTRaMCIxIDAeBgNVBAMMF0hhdGNobGluZyB0\nZXN0IENBIChSU0EpMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAvFqA\ny9JdopLwliCCMDW5XVRmZu/eWZR+/7hzwSCx+pGaZJf/AI4+tHmZXIVZJdcNROf6\nqKyc8C7XSknCVYbQPbGwHl2dsYhUGDKAWVqyKLha7hPc2s+5Mte/7FpjT/UxaccF\nvZg7QyG42SC9Yw3cK5GouUiF95SQLRl038DwnaAo9qdakOtCvecUh2jYvNpK10ja\nXI3hNHDDcZx2o2KK+9uFzIB+8vmBw/w7s6uA6fydrgxqeSKLa6EZfyd//HXdkdyx\nJ8/9gkUJnbBawfpsquGvK/GYhHmtYAcXvC9nLlkHN9JpmiAYtjPY8X6MyolOFOyY\nhZFB3cnnVHeiJUjqXwIDAQABo1MwUTAdBgNVHQ4EFgQUK7Iy52iCF9cXGlV8A5VE\nmnogr20wHwYDVR0jBBgwFoAUK7Iy52iCF9cXGlV8A5VEmnogr20wDwYDVR0TAQH/\nBAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEABihUFB7fC8mRCGg1x00P6kHCNUgq\np/ECOhxI3o66PdcwTHEFB9D1ozFknpvvHiXRzZU8LczzvcJclX42kukeXEAYCDKW\n6EYqxlTjiVycTlh5ryH/VNtaEeUrPET8CqvyozhBQqnUHYyUS/ORtIc423zC1hbh\nRbfil1OAV3GuLgNNut3bG+EqaK5W2YZU+p+LccKnRsuQdmjJtQyKrTEIxYXyGAew\nqgOY7Hj0sgqNh+ZCZ4QDUPFUostVQvIX7DO6BxnKqqDG9pngjVYJUMgGKb7+BvoT\nIKVrMdYdaQ5FpsGhmwWByOBUVJNNOWWjK9hzmNDEzFoizVuLRdNjJ4POTA==\n-----END CERTIFICATE-----";
    const RSA_CA_KEY: &str = "9X/sES7PZwM7Uktzn+dmq1vHdqK/qYvHY75DLPN9xiU=";

    /// `pem`, written to a file of its own in a scratch directory.
    fn pem_file(name: &str, pem: &str) -> (crate::api_auth::tests::Scratch, PathBuf) {
        let scratch = crate::api_auth::tests::scratch(&format!("browser-ca-{name}"));
        std::fs::create_dir_all(&scratch.0).unwrap();
        let path = scratch.0.join("ca.pem");
        std::fs::write(&path, pem).unwrap();
        (scratch, path)
    }

    #[test]
    fn the_operators_authorities_are_known_by_their_keys() {
        use base64::Engine as _;
        let der = |pem: &str| pem_certificates(pem).unwrap().remove(0);
        assert_eq!(ca_key(&der(EC_CA)).as_deref(), Some(EC_CA_KEY));
        assert_eq!(ca_key(&der(RSA_CA)).as_deref(), Some(RSA_CA_KEY));
        let both = format!("{EC_CA}\n{RSA_CA}\n");
        let (_scratch, path) = pem_file("both", &both);
        let mut vars = full();
        let path = path.display().to_string();
        vars.push(("HATCHLING_BROWSER_CA", &path));
        let settings = parse(&vars).unwrap().unwrap();
        assert_eq!(settings.ca_keys, [EC_CA_KEY, RSA_CA_KEY]);
        let dirs = settings.dirs(Path::new("/run/snapshot"), PathBuf::from("/run/h.sock"));
        assert_eq!(
            dirs.ca_keys,
            [EC_CA_KEY, RSA_CA_KEY],
            "the cell is told the keys alone"
        );
        assert!(
            parse(&full()).unwrap().unwrap().ca_keys.is_empty(),
            "none unless named"
        );
        // Not a certificate: a truncated one, another element, nothing.
        let ec = der(EC_CA);
        for broken in [
            ec.get(..ec.len() / 2).unwrap(),
            ec.get(1..).unwrap(),
            &[0x30, 0x00][..],
            &[][..],
            &[0x30, 0x84, 0, 0, 0, 1][..],
        ] {
            assert_eq!(ca_key(broken), None, "{broken:?}");
        }
        let element = der_element(&[0x04, 0x02, 0xaa, 0xbb, 0x05], 0x04).unwrap();
        assert_eq!(
            element,
            (
                &[0xaa, 0xbb][..],
                &[0x04, 0x02, 0xaa, 0xbb][..],
                &[0x05][..]
            )
        );
        assert_eq!(der_element(&[0x04, 0x02, 0xaa], 0x04), None, "short");
        let long = [&[0x04, 0x81, 0x80][..], &[0u8; 0x80][..]].concat();
        assert_eq!(
            der_element(&long, 0x04).map(|(content, _, _)| content.len()),
            Some(0x80)
        );
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(EC_CA_KEY)
                .map(|key| key.len()),
            Ok(32)
        );
    }

    #[test]
    fn a_file_of_authorities_is_read_to_their_keys() {
        let (_scratch, path) = pem_file(
            "direct",
            &format!(
                "{RSA_CA}
{EC_CA}
"
            ),
        );
        assert_eq!(
            ca_keys(&path).unwrap(),
            [RSA_CA_KEY, EC_CA_KEY],
            "in the file's order"
        );
        assert!(ca_keys(Path::new("/nonexistent/ca.pem")).is_err());
    }

    #[test]
    fn the_operators_authorities_are_refused_unless_a_readable_short_list_of_certificates() {
        let refused_with = |name: &str, pem: &str| {
            let (_scratch, path) = pem_file(name, pem);
            let mut vars = full();
            let path = path.display().to_string();
            vars.push(("HATCHLING_BROWSER_CA", &path));
            refused(&vars)
        };
        let key = "HATCHLING_BROWSER_CA";
        assert_eq!(refused_with("none", "no pem here").0, key);
        assert!(
            refused_with("none2", "no pem here")
                .1
                .contains("no certificate")
        );
        assert!(
            refused_with(
                "bad64",
                "-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----\n"
            )
            .1
            .contains("base64")
        );
        assert!(
            refused_with(
                "notcert",
                "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
            )
            .1
            .contains("doesn't parse")
        );
        let nine = format!("{EC_CA}\n").repeat(MAX_BROWSER_CAS + 1);
        assert!(refused_with("nine", &nine).1.contains("more than 8"));
        let eight = format!("{EC_CA}\n").repeat(MAX_BROWSER_CAS);
        let (_scratch, path) = pem_file("eight", &eight);
        let mut vars = full();
        let path = path.display().to_string();
        vars.push(("HATCHLING_BROWSER_CA", &path));
        assert_eq!(
            parse(&vars).unwrap().unwrap().ca_keys.len(),
            MAX_BROWSER_CAS
        );
        let big = format!(
            "{EC_CA}\n{}",
            " ".repeat(usize::try_from(MAX_BROWSER_CA_BYTES).unwrap())
        );
        assert!(refused_with("big", &big).1.contains("64 KiB"));
        assert_eq!(
            refused(
                &[
                    full(),
                    vec![("HATCHLING_BROWSER_CA", "/nonexistent/ca.pem")]
                ]
                .concat()
            ),
            (key, String::from("can't be read"))
        );
        assert_eq!(
            refused(&[full(), vec![("HATCHLING_BROWSER_CA", "ca.pem")]].concat()).0,
            key,
            "not absolute"
        );
        assert_eq!(
            refused(&[("HATCHLING_BROWSER_CA", "/srv/ca.pem")]),
            (key, String::from("is set, but HATCHLING_BROWSER isn't"))
        );
    }

    #[test]
    fn a_browser_is_configured_with_its_pin_its_controller_and_its_fonts() {
        assert_eq!(parse(&[]).unwrap(), None, "no browser, no session");
        let settings = parse(&full()).unwrap().unwrap();
        assert_eq!(
            settings.browser,
            Path::new("/opt/chrome/chrome-headless-shell")
        );
        assert_eq!(settings.sha256, SHA.to_ascii_lowercase());
        assert_eq!(
            settings.controller(),
            Path::new("/usr/lib/hatchling/hatchling-browser")
        );
        assert_eq!(
            settings.fonts.as_deref(),
            Some(Path::new("/usr/share/fonts"))
        );
        let no_fonts: Vec<_> = full().into_iter().take(3).collect();
        assert_eq!(parse(&no_fonts).unwrap().unwrap().fonts(), None);
        assert_eq!(settings.fonts(), Some(Path::new("/usr/share/fonts")));
        let dirs = settings.dirs(Path::new("/run/snapshot"), PathBuf::from("/run/h.sock"));
        assert_eq!(dirs.nss, nss_modules(Path::is_file), "as the host has them");
        assert_eq!(
            BrowserDirs {
                nss: Vec::new(),
                ..dirs.clone()
            },
            BrowserDirs {
                browser_dir: PathBuf::from("/run/snapshot"),
                browser_name: String::from("chrome-headless-shell"),
                fonts: Some(PathBuf::from("/usr/share/fonts")),
                nss: Vec::new(),
                ca_keys: Vec::new(),
                relay: RELAY_PORT,
                handoff: PathBuf::from("/run/h.sock"),
            }
        );
        assert_eq!(
            dirs.browser(),
            Path::new("/run/snapshot/chrome-headless-shell")
        );
        assert_eq!(settings.bundle(), Path::new("/opt/chrome"));
    }

    #[test]
    fn nss_modules_come_from_where_nss_is() {
        assert_eq!(host_nss_modules(), nss_modules(Path::is_file));
        let host = |files: &'static [&'static str]| {
            move |path: &Path| files.iter().any(|file| path == Path::new(file))
        };
        assert_eq!(
            nss_modules(host(&[
                "/usr/lib/libnss3.so",
                "/usr/lib/libsoftokn3.so",
                "/usr/lib/libfreeblpriv3.so",
                "/usr/lib64/libsoftokn3.so",
            ])),
            [
                PathBuf::from("/usr/lib/libsoftokn3.so"),
                PathBuf::from("/usr/lib/libfreeblpriv3.so")
            ],
            "beside NSS, not wherever a module is"
        );
        assert_eq!(
            nss_modules(host(&[
                "/usr/lib/x86_64-linux-gnu/libnss3.so",
                "/usr/lib/x86_64-linux-gnu/libsoftokn3.so",
                "/usr/lib/libnss3.so",
                "/usr/lib/libfreeblpriv3.so",
            ])),
            [PathBuf::from("/usr/lib/x86_64-linux-gnu/libsoftokn3.so")],
            "the first directory with NSS, and only the modules it has"
        );
        assert!(
            nss_modules(host(&["/usr/lib/libsoftokn3.so"])).is_empty(),
            "no NSS, no modules"
        );
    }

    #[test]
    fn a_setting_that_isnt_valid_is_refused_by_name() {
        let with = |key: &'static str, value: &'static str| {
            let mut vars = full();
            vars.retain(|(k, _)| *k != key);
            if !value.is_empty() {
                vars.push((key, value));
            }
            vars
        };
        for key in [
            "HATCHLING_BROWSER_SHA256",
            "HATCHLING_BROWSER_CONTROLLER",
            "HATCHLING_BROWSER_FONTS",
        ] {
            assert_eq!(
                refused(&[(key, "/x")]),
                (key, String::from("is set, but HATCHLING_BROWSER isn't")),
            );
        }
        for (key, value, reason) in [
            ("HATCHLING_BROWSER", "chrome", "isn't an absolute path"),
            ("HATCHLING_BROWSER", "/", "names no file"),
            (
                "HATCHLING_BROWSER_SHA256",
                "",
                "is required with HATCHLING_BROWSER",
            ),
            ("HATCHLING_BROWSER_SHA256", "abc", "isn't 64 hex digits"),
            (
                "HATCHLING_BROWSER_SHA256",
                "zz0ffc88be1c6f7dc8351bebfaca965ff9a07617bddf696f0b67ef85e5868b29a",
                "isn't 64 hex digits",
            ),
            (
                "HATCHLING_BROWSER_CONTROLLER",
                "",
                "is required with HATCHLING_BROWSER",
            ),
            (
                "HATCHLING_BROWSER_CONTROLLER",
                "bin/c",
                "isn't an absolute path",
            ),
            ("HATCHLING_BROWSER_FONTS", "fonts", "isn't an absolute path"),
        ] {
            assert_eq!(
                refused(&with(key, value)),
                (key, String::from(reason)),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn a_browser_runs_only_from_a_snapshot_of_its_pinned_bundle() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-browser-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chrome"), b"the pinned browser").unwrap();
        std::fs::write(dir.join("icudtl.dat"), b"icu").unwrap();
        let pinned = crate::browser_bundle::digest(&dir).unwrap();
        let settings = |sha: &str| BrowserSettings {
            browser: dir.join("chrome"),
            sha256: sha.to_owned(),
            controller: PathBuf::from("/c"),
            fonts: None,
            ca_keys: Vec::new(),
        };
        assert_eq!(settings(&pinned).bundle(), dir);
        let snapshot = settings(&pinned).snapshot().unwrap();
        assert_eq!(
            std::fs::read(snapshot.dir().join("icudtl.dat")).unwrap(),
            b"icu"
        );
        // Not the executable alone: any file of the bundle that changes
        // makes it another bundle, never started.
        std::fs::write(dir.join("icudtl.dat"), b"ICU").unwrap();
        let changed = settings(&pinned).snapshot().unwrap_err();
        assert!(changed.contains("it is never started"), "{changed}");
        assert!(changed.contains(&pinned), "{changed}");
        // The snapshot made before is the bundle as it was checked.
        assert_eq!(
            std::fs::read(snapshot.dir().join("icudtl.dat")).unwrap(),
            b"icu"
        );
        drop(snapshot);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_browser_cell_is_bounded_in_memory_and_processes() {
        let limits = browser_limits();
        assert_eq!(limits.memory, Some(2 << 30));
        assert_eq!(limits.processes, Some(512));
        assert_eq!(BROWSER_SCRATCH.get(), 512 << 20);
        assert!(
            BROWSER_SCRATCH.get() < BROWSER_MEMORY,
            "within the cell's memory"
        );
        assert_eq!(limits.file_size, CellLimits::default().file_size);
        assert_eq!(limits.open_files, CellLimits::default().open_files);
    }
}
