//! A browser session's configuration (WO-029): the operator's pinned
//! browser, the controller that drives it, and the fonts it lays out text
//! with.
//!
//! Hatchling bundles no browser. The operator names one
//! (`HATCHLING_BROWSER`) and its SHA-256 (`HATCHLING_BROWSER_SHA256`),
//! and the binary is hashed again before every start: one that no longer
//! matches is never started. The controller
//! (`HATCHLING_BROWSER_CONTROLLER`) is Hatchling's own program, like the
//! agent's binary. Fonts (`HATCHLING_BROWSER_FONTS`) are a host directory
//! the cell sees read-only; without them the browser can't lay out text.

use std::path::{Path, PathBuf};

use hatchling_skill::CodeHash;

use crate::Error;
use crate::browser_proxy::RELAY_PORT;
use crate::cell::{BrowserDirs, CellLimits, env_value};
use crate::environment as env;

/// A browser cell's memory, as a whole: 2 GiB.
pub const BROWSER_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

/// A browser cell's processes, threads included: a browser runs several
/// processes of many threads each.
pub const BROWSER_PROCESSES: u64 = 512;

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
    /// The settings in the broker's environment: `None` when no browser
    /// is configured.
    ///
    /// # Errors
    ///
    /// As [`BrowserSettings::parse`].
    pub fn from_env() -> Result<Option<Self>, Error> {
        Self::parse(|key| env_value(std::env::var(key)))
    }

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

    /// Whether the browser binary still hashes to its pin: read now, a
    /// regular file and not a symlink. Called before every start.
    ///
    /// # Errors
    ///
    /// A sentence for the log: the browser is unreadable, a symlink, or no
    /// longer the pinned one.
    pub fn verified(&self) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(&self.browser).map_err(|err| {
            format!(
                "the browser {} is unreadable: {err}",
                self.browser.display()
            )
        })?;
        if !meta.file_type().is_file() {
            return Err(format!(
                "the browser {} is not a regular file",
                self.browser.display()
            ));
        }
        let bytes = std::fs::read(&self.browser).map_err(|err| {
            format!(
                "the browser {} is unreadable: {err}",
                self.browser.display()
            )
        })?;
        let actual = CodeHash::of(&bytes).to_string();
        if actual == self.sha256 {
            Ok(())
        } else {
            Err(format!(
                "the browser's SHA-256 is {actual}, not the pinned {}: it is never started",
                self.sha256
            ))
        }
    }

    /// A browser cell's own part, with its scratch directory `scratch`
    /// and the relay handed over at `handoff`.
    #[must_use]
    pub fn dirs(&self, scratch: PathBuf, handoff: PathBuf) -> BrowserDirs {
        BrowserDirs {
            browser_dir: self
                .browser
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf),
            browser_name: self
                .browser
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            fonts: self.fonts.clone(),
            scratch,
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
        let dirs = settings.dirs(PathBuf::from("/run/s"), PathBuf::from("/run/h.sock"));
        assert_eq!(
            dirs,
            BrowserDirs {
                browser_dir: PathBuf::from("/opt/chrome"),
                browser_name: String::from("chrome-headless-shell"),
                fonts: Some(PathBuf::from("/usr/share/fonts")),
                scratch: PathBuf::from("/run/s"),
                relay: RELAY_PORT,
                handoff: PathBuf::from("/run/h.sock"),
            }
        );
        assert_eq!(
            dirs.browser(),
            Path::new("/opt/chrome/chrome-headless-shell")
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
    fn a_browser_is_started_only_while_it_hashes_to_its_pin() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-browser-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("chrome");
        std::fs::write(&binary, b"the pinned browser").unwrap();
        let pinned = CodeHash::of(b"the pinned browser").to_string();
        let settings = |path: &Path, sha: &str| BrowserSettings {
            browser: path.to_owned(),
            sha256: sha.to_owned(),
            controller: PathBuf::from("/c"),
            fonts: None,
        };
        assert_eq!(settings(&binary, &pinned).verified(), Ok(()));
        std::fs::write(&binary, b"another browser").unwrap();
        let changed = settings(&binary, &pinned).verified().unwrap_err();
        assert!(changed.contains("it is never started"), "{changed}");
        assert!(changed.contains(&pinned), "{changed}");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&binary, &link).unwrap();
        let symlink = settings(&link, &pinned).verified().unwrap_err();
        assert!(symlink.contains("not a regular file"), "{symlink}");
        let missing = settings(&dir.join("gone"), &pinned).verified().unwrap_err();
        assert!(missing.contains("unreadable"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_browser_cell_is_bounded_in_memory_and_processes() {
        let limits = browser_limits();
        assert_eq!(limits.memory, Some(2 << 30));
        assert_eq!(limits.processes, Some(512));
        assert_eq!(limits.file_size, CellLimits::default().file_size);
        assert_eq!(limits.open_files, CellLimits::default().open_files);
    }
}
