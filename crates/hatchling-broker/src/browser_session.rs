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

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::browser_proxy::RELAY_PORT;
use crate::cell::{BrowserDirs, CellLimits};
use crate::environment as env;

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
        let Some(browser) = browser else {
            for (key, value) in [
                (env::HATCHLING_BROWSER_SHA256.name, &sha256),
                (env::HATCHLING_BROWSER_CONTROLLER.name, &controller),
                (env::HATCHLING_BROWSER_FONTS.name, &fonts),
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
        Ok(Some(Self {
            browser,
            sha256,
            controller,
            fonts,
        }))
    }

    /// The controller, the cell's program.
    #[must_use]
    pub fn controller(&self) -> &Path {
        &self.controller
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
            nss: nss_modules(Path::is_file),
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
        assert_eq!(parse(&no_fonts).unwrap().unwrap().fonts, None);
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
