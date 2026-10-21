//! `hatchling-browser`: the controller a browser cell runs. See the
//! `hatchling_browser` crate docs.

use std::io::Write as _;

fn main() -> std::process::ExitCode {
    let done = hatchling_browser::Config::from(|key| std::env::var(key).ok())
        .and_then(|config| hatchling_browser::run(&config));
    match done {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "hatchling-browser: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
