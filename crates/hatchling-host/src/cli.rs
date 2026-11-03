//! The host's command line.
//!
//! Thin by design, as the other commands are: settings, the token, the
//! listener, then serve. Output goes through `io::Write`; the outcome is
//! a returned [`ExitCode`].

use std::io::Write as _;
use std::process::ExitCode;

use hatchling_core::cli::{EX_USAGE, help_requested, render_help};

use crate::{Error, Settings, address, bind, machine_address, read_token, serve};

/// Runs the host's command line: `args` after the program's name.
#[must_use]
pub fn main(program: &str, args: &[String]) -> ExitCode {
    if help_requested(args) {
        let _ = write!(std::io::stdout().lock(), "{}", help(program));
        return ExitCode::SUCCESS;
    }
    match args {
        [] => {}
        [url] if url == "url" => return print_url(program),
        _ => {
            let _ = write!(std::io::stderr().lock(), "{}", help(program));
            return ExitCode::from(EX_USAGE);
        }
    }
    match start() {
        Ok(never) => match never {},
        Err(error) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program}: failed to start: {error}"
            );
            ExitCode::FAILURE
        }
    }
}

/// The host's help, from its settings' declarations.
#[must_use]
pub fn help(program: &str) -> String {
    render_help(
        program,
        "[url] | --help | -h",
        "Serves Hatchling's UI on 127.0.0.1 and carries its channel to the broker's\nUI socket, message for message. It prints the address to open: the token in\nits fragment is what the broker asks of every UI connection, and a browser\nnever sends a fragment to a server. With the operator socket, it serves the\nMachine view too, at an address of its own. The host decides nothing; the\nbroker does.\n\nurl prints the address to open now. The broker makes a new token each time\nit starts, so after a restart an open app goes offline: open this address\ninstead. With no settings, it reads the installed broker's socket.",
        crate::environment::ENVIRONMENT,
    )
}

/// Prints the UI's address with the broker's token now, or why it can't.
fn print_url(program: &str) -> ExitCode {
    match url(|key| std::env::var(key).ok()) {
        Ok(address) => {
            let _ = writeln!(std::io::stdout().lock(), "{address}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "{program}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The UI's address with the broker's token now: the socket and port
/// `get` names, or the installed broker's socket and the default port.
fn url(get: impl Fn(&str) -> Option<String>) -> Result<String, Error> {
    let socket = get(crate::environment::HATCHLING_UI_SOCKET.name).map_or_else(
        || std::path::PathBuf::from(INSTALLED_UI_SOCKET),
        std::path::PathBuf::from,
    );
    let port = match get(crate::environment::HATCHLING_HOST_PORT.name) {
        None => crate::DEFAULT_PORT,
        Some(raw) => raw
            .trim()
            .parse()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| Error::Invalid {
                key: "HATCHLING_HOST_PORT",
                reason: format!("must be the port the host serves on, not {raw:?}"),
            })?,
    };
    Ok(address(port, &read_token(&socket, "HATCHLING_UI_SOCKET")?))
}

/// Where `scripts/install.sh` has the broker put its UI socket.
const INSTALLED_UI_SOCKET: &str = "/var/lib/hatchling/ui.sock";

/// Reads the settings and the token, binds, says where to go, and serves.
fn start() -> Result<std::convert::Infallible, Error> {
    let settings = Settings::parse(|key| std::env::var(key).ok())?;
    let token = read_token(&settings.ui_socket, "HATCHLING_UI_SOCKET")?;
    let operator = settings
        .operator_socket
        .as_deref()
        .map(|socket| read_token(socket, "HATCHLING_OPERATOR_SOCKET"))
        .transpose()?;
    let listener = bind(settings.port)?;
    let port = listener.local_addr()?.port();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "Hatchling's UI: open {}", address(port, &token));
    if let Some(operator) = operator {
        let _ = writeln!(
            out,
            "Hatchling's Machine view: open {}",
            machine_address(port, &operator)
        );
    }
    drop(out);
    serve(&listener, &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_reads_the_token_now_from_the_socket_named_or_installed() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-url-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ui.sock");
        let named = socket.to_str().unwrap().to_owned();
        let get = |port: Option<&'static str>| {
            let named = named.clone();
            move |key: &str| match key {
                "HATCHLING_UI_SOCKET" => Some(named.clone()),
                "HATCHLING_HOST_PORT" => port.map(String::from),
                _ => None,
            }
        };
        assert!(url(get(None)).is_err(), "no token yet");
        std::fs::write(crate::token_path(&socket), "first\n").unwrap();
        assert_eq!(
            url(get(None)).unwrap(),
            "http://127.0.0.1:7340/#hatchling-ui=first"
        );
        // A restart's new token, read again.
        std::fs::write(crate::token_path(&socket), "second\n").unwrap();
        assert_eq!(
            url(get(Some("7400"))).unwrap(),
            "http://127.0.0.1:7400/#hatchling-ui=second"
        );
        for port in ["0", "x", "70000"] {
            assert!(
                matches!(
                    url(get(Some(port))),
                    Err(Error::Invalid {
                        key: "HATCHLING_HOST_PORT",
                        ..
                    })
                ),
                "{port}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
        // With no settings, the installed broker's socket.
        let installed = url(|_| None).unwrap_err().to_string();
        assert!(
            installed.contains("/var/lib/hatchling/ui.sock.token"),
            "{installed}"
        );
    }

    /// `url` prints the address, anything else is misuse, and no
    /// arguments serve, which fails at once without the host's settings.
    /// A test process has none of the host's settings, so `url` reads the
    /// installed broker's token: there only where Hatchling is installed.
    #[test]
    fn main_serves_prints_the_url_or_refuses() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        if std::env::var_os("HATCHLING_UI_SOCKET").is_some() {
            return;
        }
        assert_eq!(main("hatchling-host", &args(&[])), ExitCode::FAILURE);
        let installed = crate::token_path(std::path::Path::new(INSTALLED_UI_SOCKET)).exists();
        assert_eq!(
            main("hatchling-host", &args(&["url"])),
            if installed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        );
        for misuse in [&["serve"][..], &["url", "now"], &["URL"]] {
            assert_eq!(
                main("hatchling-host", &args(misuse)),
                ExitCode::from(EX_USAGE),
                "{misuse:?}"
            );
        }
    }

    #[test]
    fn help_names_every_setting_and_the_usage() {
        let text = help("hatchling-host");
        assert!(text.starts_with("Usage: hatchling-host [url] | --help | -h\n"));
        for variable in crate::environment::ENVIRONMENT {
            assert!(text.contains(variable.name), "{}", variable.name);
        }
    }
}
