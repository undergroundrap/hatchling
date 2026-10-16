//! The host's command line.
//!
//! Thin by design, as the other commands are: settings, the token, the
//! listener, then serve. Output goes through `io::Write`; the outcome is
//! a returned [`ExitCode`].

use std::io::Write as _;
use std::process::ExitCode;

use hatchling_core::cli::{EX_USAGE, help_requested, render_help};

use crate::{Error, Settings, address, bind, read_token, serve};

/// Runs the host's command line: `args` after the program's name.
#[must_use]
pub fn main(program: &str, args: &[String]) -> ExitCode {
    if help_requested(args) {
        let _ = write!(std::io::stdout().lock(), "{}", help(program));
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        let _ = write!(std::io::stderr().lock(), "{}", help(program));
        return ExitCode::from(EX_USAGE);
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
        "| --help | -h",
        "Serves Hatchling's UI on 127.0.0.1 and carries its channel to the broker's\nUI socket, message for message. It prints the address to open: the token in\nits fragment is what the broker asks of every UI connection, and a browser\nnever sends a fragment to a server. The host decides nothing; the broker does.",
        crate::environment::ENVIRONMENT,
    )
}

/// Reads the settings and the token, binds, says where to go, and serves.
fn start() -> Result<std::convert::Infallible, Error> {
    let settings = Settings::parse(|key| std::env::var(key).ok())?;
    let token = read_token(&settings.ui_socket)?;
    let listener = bind(settings.port)?;
    let port = listener.local_addr()?.port();
    let _ = writeln!(
        std::io::stdout().lock(),
        "Hatchling's UI: open {}",
        address(port, &token)
    );
    serve(&listener, &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_names_every_setting_and_the_usage() {
        let text = help("hatchling-host");
        assert!(text.starts_with("Usage: hatchling-host | --help | -h\n"));
        for variable in crate::environment::ENVIRONMENT {
            assert!(text.contains(variable.name), "{}", variable.name);
        }
    }
}
