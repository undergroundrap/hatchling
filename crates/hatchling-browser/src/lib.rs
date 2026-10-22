//! `hatchling-browser`: the controller that runs in a browser cell
//! (WO-029). It starts a headless Chromium, drives it over the DevTools
//! protocol on a pipe (never a port), and answers the broker's commands
//! on the cell's one connection with snapshots of the page.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling).
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! # Not trusted
//!
//! The controller runs beside page code: Chromium runs without its own
//! sandbox, which can't run in a cell, so a renderer exploit can reach
//! the controller. Nothing here is a boundary. The cell is (WO-029), and
//! the broker, which bounds every reply it reads and decides every site.
//! What the controller does keep to:
//!
//! - **The pipe, never a port.** No page can reach a pipe.
//! - **Nothing of its own crosses into the browser.** Chromium gets
//!   descriptors 0 to 4: stdio and the pipe ([`launch`]).
//! - **Every wait is bounded,** and every message read is bounded
//!   ([`cdp`]): a browser that stops answering, or a child of it killed,
//!   costs a timeout, never a hang.
//! - **A snapshot is read where the page can't reach it,** in an isolated
//!   world, and bounded to one frame ([`page`]).

pub mod cdp;
pub mod launch;
pub mod page;
pub mod serve;

use std::path::{Path, PathBuf};

use crate::launch::{Launch, chromium_args, launch};
use crate::page::Session;

/// The descriptor of the cell's connection to the broker.
pub const CONNECTION_FD: &str = "HATCHLING_CONNECTION_FD";
/// The browser, as the cell sees it.
pub const BROWSER: &str = "HATCHLING_BROWSER";
/// The cell's relay port: the broker's proxy, on the cell's loopback.
pub const PROXY_PORT: &str = "HATCHLING_BROWSER_PROXY_PORT";
/// The browser's profile directory, in the cell's temporary storage.
pub const PROFILE: &str = "HATCHLING_BROWSER_PROFILE";
/// A directory of fonts the cell sees.
///
/// Without fonts, Chromium's renderer can't lay out text. The cell has no
/// `/etc/fonts`, so the controller writes the font configuration itself,
/// naming this directory alone.
pub const FONTS: &str = "HATCHLING_BROWSER_FONTS";
/// The operator's certificate authorities, by their keys' SHA-256 in
/// standard base64, comma-separated: the browser trusts a chain holding one.
pub const CA_KEYS: &str = "HATCHLING_BROWSER_CA_KEYS";

/// What the controller is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The connection to the broker.
    pub connection: i32,
    /// The browser.
    pub browser: PathBuf,
    /// The proxy port, if the cell has one.
    pub proxy: Option<u16>,
    /// The profile directory.
    pub profile: PathBuf,
    /// The fonts directory, if the cell has one.
    pub fonts: Option<PathBuf>,
    /// The operator's certificate authorities, by their keys.
    pub ca_keys: Vec<String>,
    /// The cell's temporary directory: the font configuration and its
    /// cache are written here.
    pub tmp: PathBuf,
    /// The browser's environment: the cell's home and temporary
    /// directory, and nothing else.
    pub env: Vec<(String, String)>,
}

/// Why the controller couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A setting is missing or isn't valid.
    #[error("{name}: {reason}")]
    Config {
        /// The setting.
        name: &'static str,
        /// What's wrong with it.
        reason: &'static str,
    },
    /// The connection to the broker couldn't be taken.
    #[error("the broker connection: {0}")]
    Connection(hatchling_sys::Error),
    /// The font configuration couldn't be written.
    #[error("the font configuration: {0}")]
    Fonts(std::io::Error),
    /// The browser couldn't be started.
    #[error(transparent)]
    Launch(#[from] launch::LaunchError),
    /// Serving stopped.
    #[error(transparent)]
    Serve(#[from] serve::ServeError),
}

impl Config {
    /// The configuration in `get`'s variables.
    ///
    /// # Errors
    ///
    /// [`RunError::Config`] for a missing connection or browser, or one of
    /// the settings not valid.
    pub fn from(get: impl Fn(&str) -> Option<String>) -> Result<Self, RunError> {
        let invalid = |name, reason| RunError::Config { name, reason };
        let connection = get(CONNECTION_FD)
            .ok_or_else(|| invalid(CONNECTION_FD, "not set"))?
            .parse::<i32>()
            .ok()
            .filter(|fd| *fd > 2)
            .ok_or_else(|| invalid(CONNECTION_FD, "not a descriptor above stderr"))?;
        let browser = get(BROWSER)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid(BROWSER, "not an absolute path"))?;
        let proxy = get(PROXY_PORT)
            .map(|port| {
                port.parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| invalid(PROXY_PORT, "not a port"))
            })
            .transpose()?;
        let tmp = get("TMPDIR").unwrap_or_else(|| String::from("/tmp"));
        let profile =
            get(PROFILE).map_or_else(|| PathBuf::from(&tmp).join("profile"), PathBuf::from);
        if !profile.is_absolute() {
            return Err(invalid(PROFILE, "not an absolute path"));
        }
        let fonts = get(FONTS)
            .map(|dir| {
                Some(PathBuf::from(dir))
                    .filter(|dir| dir.is_absolute() && plain(dir))
                    .ok_or_else(|| invalid(FONTS, "not an absolute plain path"))
            })
            .transpose()?;
        let ca_keys: Vec<String> = get(CA_KEYS)
            .map(|keys| keys.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        if !ca_keys.iter().all(|key| is_key(key)) {
            return Err(invalid(CA_KEYS, "not keys' SHA-256 in base64"));
        }
        if !plain(Path::new(&tmp)) {
            return Err(invalid("TMPDIR", "not a plain path"));
        }
        let home = get("HOME").unwrap_or_else(|| tmp.clone());
        let mut env = vec![
            (String::from("HOME"), home),
            (String::from("TMPDIR"), tmp.clone()),
        ];
        if fonts.is_some() {
            env.push((
                String::from("FONTCONFIG_FILE"),
                Path::new(&tmp).join("fonts.conf").display().to_string(),
            ));
        }
        Ok(Self {
            connection,
            browser,
            proxy,
            profile,
            fonts,
            ca_keys,
            tmp: PathBuf::from(tmp),
            env,
        })
    }
}

/// Whether `key` is a SHA-256 in standard base64: 43 of its characters and
/// one `=`. Nothing else reaches the browser's command line.
fn is_key(key: &str) -> bool {
    key.len() == 44
        && key.ends_with('=')
        && key
            .bytes()
            .take(43)
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

/// Whether `path` is one a font configuration can name as it is: no
/// character XML would read as markup.
fn plain(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|path| !path.contains(['<', '>', '&', '"', '\'']))
}

/// The font configuration for `fonts`, its cache in `cache`: those two
/// directories, and nothing of the host's.
#[must_use]
pub fn fonts_conf(fonts: &Path, cache: &Path) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n<fontconfig>\n  <dir>{}</dir>\n  <cachedir>{}</cachedir>\n</fontconfig>\n",
        fonts.display(),
        cache.display()
    )
}

/// Runs the controller: the browser started, the broker served until it's
/// done.
///
/// # Errors
///
/// [`RunError`], for a setting, the connection, the browser or serving.
pub fn run(config: &Config) -> Result<(), RunError> {
    let connection = hatchling_sys::net::inherited_unix_stream(config.connection)
        .map_err(RunError::Connection)?;
    if let Some(fonts) = &config.fonts {
        let conf = fonts_conf(fonts, &config.tmp.join("fontconfig"));
        std::fs::write(config.tmp.join("fonts.conf"), conf).map_err(RunError::Fonts)?;
    }
    let browser = launch(&Launch {
        program: config.browser.clone(),
        args: chromium_args(&config.profile, config.proxy, &config.ca_keys),
        env: config.env.clone(),
    })?;
    serve::serve(&connection, Session::new(browser))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, RunError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from(|key| vars.get(key).cloned())
    }

    fn refused(vars: &[(&str, &str)]) -> (&'static str, &'static str) {
        match config(vars) {
            Err(RunError::Config { name, reason }) => (name, reason),
            Ok(_) => ("not refused", ""),
            Err(_) => ("refused otherwise", ""),
        }
    }

    #[test]
    fn the_operators_authorities_reach_the_browser_only_as_keys() {
        let fd = (CONNECTION_FD, "3");
        let browser = (BROWSER, "/browser/chrome-headless-shell");
        assert!(config(&[fd, browser]).unwrap().ca_keys.is_empty(), "none unless named");
        let one = "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYxGE=";
        let two = "9X/sES7PZwM7Uktzn+dmq1vHdqK/qYvHY75DLPN9xiU=";
        let both = format!("{one},{two}");
        let named = config(&[fd, browser, (CA_KEYS, &both)]).unwrap();
        assert_eq!(named.ca_keys, [one, two]);
        for bad in [
            "",
            "short=",
            "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYxGE",
            "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYxG==",
            "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYxG -=",
            "j5vFg5J9HEaJMQfN82FoLOYU6/odDW9zY6eNDAkYx,G=",
        ] {
            assert_eq!(
                refused(&[fd, browser, (CA_KEYS, bad)]),
                (CA_KEYS, "not keys' SHA-256 in base64"),
                "{bad:?}"
            );
        }
        assert!(is_key(one) && is_key(two));
        assert!(!is_key("--disable-web-security=AAAAAAAAAAAAAAAAAAAAAAAAA="), "never a flag");
    }

    #[test]
    fn the_controller_is_configured_by_the_cell() {
        let full = config(&[
            (CONNECTION_FD, "3"),
            (BROWSER, "/browser/chrome-headless-shell"),
            (PROXY_PORT, "8080"),
            ("TMPDIR", "/scratch"),
            ("HOME", "/scratch/home"),
        ])
        .unwrap();
        assert_eq!(full.connection, 3);
        assert_eq!(full.browser, Path::new("/browser/chrome-headless-shell"));
        assert_eq!(full.proxy, Some(8080));
        assert_eq!(full.profile, Path::new("/scratch/profile"));
        assert_eq!(
            full.env,
            [
                (String::from("HOME"), String::from("/scratch/home")),
                (String::from("TMPDIR"), String::from("/scratch")),
            ]
        );
        let least = config(&[(CONNECTION_FD, "9"), (BROWSER, "/b")]).unwrap();
        assert_eq!(least.proxy, None);
        assert_eq!(least.profile, Path::new("/tmp/profile"));
        assert_eq!(
            least.env,
            [
                (String::from("HOME"), String::from("/tmp")),
                (String::from("TMPDIR"), String::from("/tmp")),
            ]
        );
        let named = config(&[(CONNECTION_FD, "9"), (BROWSER, "/b"), (PROFILE, "/p")]).unwrap();
        assert_eq!(named.profile, Path::new("/p"));
        assert_eq!(named.fonts, None);
        let fonts = config(&[
            (CONNECTION_FD, "9"),
            (BROWSER, "/b"),
            (FONTS, "/fonts"),
            ("TMPDIR", "/scratch"),
        ])
        .unwrap();
        assert_eq!(fonts.fonts.as_deref(), Some(Path::new("/fonts")));
        assert_eq!(fonts.tmp, Path::new("/scratch"));
        assert_eq!(
            fonts.env.last(),
            Some(&(
                String::from("FONTCONFIG_FILE"),
                String::from("/scratch/fonts.conf")
            ))
        );
    }

    #[test]
    fn a_setting_that_isnt_valid_is_refused_by_name() {
        assert_eq!(refused(&[(BROWSER, "/b")]), (CONNECTION_FD, "not set"));
        for fd in ["2", "-1", "x"] {
            assert_eq!(
                refused(&[(CONNECTION_FD, fd), (BROWSER, "/b")]),
                (CONNECTION_FD, "not a descriptor above stderr"),
                "{fd}"
            );
        }
        assert_eq!(
            refused(&[(CONNECTION_FD, "3")]),
            (BROWSER, "not an absolute path")
        );
        assert_eq!(
            refused(&[(CONNECTION_FD, "3"), (BROWSER, "chrome")]),
            (BROWSER, "not an absolute path")
        );
        for port in ["0", "65536", "x"] {
            assert_eq!(
                refused(&[(CONNECTION_FD, "3"), (BROWSER, "/b"), (PROXY_PORT, port)]),
                (PROXY_PORT, "not a port"),
                "{port}"
            );
        }
        assert_eq!(
            refused(&[(CONNECTION_FD, "3"), (BROWSER, "/b"), (PROFILE, "p")]),
            (PROFILE, "not an absolute path")
        );
        for dir in ["fonts", "/fonts<x>", "/a&b", "/q\"", "/q'"] {
            assert_eq!(
                refused(&[(CONNECTION_FD, "3"), (BROWSER, "/b"), (FONTS, dir)]),
                (FONTS, "not an absolute plain path"),
                "{dir}"
            );
        }
        assert_eq!(
            refused(&[(CONNECTION_FD, "3"), (BROWSER, "/b"), ("TMPDIR", "/t<")]),
            ("TMPDIR", "not a plain path")
        );
    }

    #[test]
    fn the_font_configuration_names_the_cells_fonts_alone() {
        let conf = fonts_conf(Path::new("/fonts"), Path::new("/scratch/fontconfig"));
        assert!(conf.starts_with("<?xml version=\"1.0\"?>\n"), "{conf}");
        assert!(conf.contains("<dir>/fonts</dir>"), "{conf}");
        assert!(
            conf.contains("<cachedir>/scratch/fontconfig</cachedir>"),
            "{conf}"
        );
        assert_eq!(
            conf.matches("<dir>").count(),
            1,
            "no host directory: {conf}"
        );
        assert!(!conf.contains("<include"), "nothing of the host's: {conf}");
    }
}
