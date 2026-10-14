//! The development command line; help and usage precede every check.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use hatchling_core::cli::{EX_USAGE, help_requested, render_help};

use crate::{Error, dispatch, workspace_root};

/// Runs checks only after handling help and validating arguments.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    if help_requested(args) {
        let _ = write!(std::io::stdout().lock(), "{}", help());
        return ExitCode::SUCCESS;
    }
    if !valid_args(args) {
        let _ = write!(std::io::stderr().lock(), "{}", help());
        return ExitCode::from(EX_USAGE);
    }
    let command = args.first().map_or("audit", String::as_str);
    match run(
        command,
        args.get(1..).unwrap_or_default(),
        &workspace_root(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "hatchling-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Lists commands and settings without inspecting the workspace.
#[must_use]
pub fn help() -> String {
    render_help(
        "hatchling-dev",
        "[command] | --help | -h",
        "Development-only checks; never part of the shipped runtime.\nCommands:\n  audit                         fast checks (the default)\n  mutants                       mutation gate\n  mutants-cached                mutation gate with the content-hash cache\n  coverage                      coverage gate\n  seed-cache --from <file> --out <file>\n  preflight --base <commit> [--root <directory>]\n  protocol-schema [--write]      verify or generate the protocol schema\n  seed-check                    verify the fixed cache fixture",
        crate::environment::ENVIRONMENT,
    )
}

fn valid_args(args: &[String]) -> bool {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        None => true,
        Some("audit" | "mutants" | "mutants-cached" | "coverage" | "seed-check") => rest.is_empty(),
        Some("protocol-schema") => rest.is_empty() || matches!(rest, [flag] if flag == "--write"),
        Some("seed-cache") => flags_valid(rest, &["--from", "--out"], &["--from", "--out"]),
        Some("preflight") => flags_valid(rest, &["--base", "--root"], &["--base"]),
        Some(_) => false,
    }
}

fn flags_valid(args: &[String], allowed: &[&str], required: &[&str]) -> bool {
    let mut seen = HashSet::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if !allowed.contains(&flag.as_str()) || !seen.insert(flag.as_str()) || iter.next().is_none()
        {
            return false;
        }
    }
    required.iter().all(|flag| seen.contains(flag))
}

fn run(command: &str, rest: &[String], root: &Path) -> Result<(), Error> {
    let findings = dispatch(command, rest, root)?;
    let mut out = std::io::stdout().lock();
    if findings.is_empty() {
        let _ = writeln!(out, "hatchling-dev {command}: all checks passed");
        Ok(())
    } else {
        for finding in &findings {
            let _ = writeln!(out, "[{}] {}", finding.check, finding.message);
        }
        Err(Error::ToolFailed {
            tool: format!("hatchling-dev {command}"),
            status: format!("{} finding(s)", findings.len()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn help_and_misuse_never_start_a_check() {
        for flag in ["--help", "-h"] {
            assert_eq!(main(&args(&[flag])), ExitCode::SUCCESS);
        }
        for given in [
            &["bogus"][..],
            &["audit", "--unknown"],
            &["--help", "extra"],
        ] {
            assert_eq!(main(&args(given)), ExitCode::from(EX_USAGE));
        }
        let text = help();
        for command in [
            "audit",
            "mutants",
            "mutants-cached",
            "coverage",
            "seed-cache",
            "preflight",
            "protocol-schema",
            "seed-check",
        ] {
            assert!(text.contains(command), "{command}");
        }
        for variable in crate::environment::ENVIRONMENT {
            assert!(text.contains(variable.name));
            assert!(text.contains(variable.requirement));
            assert!(text.contains(variable.description));
        }
    }

    #[test]
    fn valid_args_checks_commands_and_required_values() {
        for given in [
            vec![],
            args(&["audit"]),
            args(&["mutants"]),
            args(&["mutants-cached"]),
            args(&["coverage"]),
            args(&["seed-check"]),
            args(&["protocol-schema"]),
            args(&["protocol-schema", "--write"]),
            args(&["seed-cache", "--out", "out", "--from", "in"]),
            args(&["preflight", "--base", "main"]),
            args(&["preflight", "--root", "/work", "--base", "main"]),
        ] {
            assert!(valid_args(&given), "{given:?}");
        }
        for given in [
            args(&["bogus"]),
            args(&["audit", "extra"]),
            args(&["mutants", "extra"]),
            args(&["mutants-cached", "extra"]),
            args(&["coverage", "extra"]),
            args(&["seed-check", "extra"]),
            args(&["protocol-schema", "--unknown"]),
            args(&["seed-cache", "--from", "in"]),
            args(&["preflight", "--root", "/work"]),
        ] {
            assert!(!valid_args(&given), "{given:?}");
        }
    }

    #[test]
    fn flags_valid_refuses_unknown_duplicate_and_unpaired_flags() {
        assert!(flags_valid(
            &args(&["--base", "main"]),
            &["--base"],
            &["--base"]
        ));
        for given in [
            vec![],
            args(&["--base"]),
            args(&["--other", "value"]),
            args(&["--base", "main", "--base", "other"]),
        ] {
            assert!(!flags_valid(&given, &["--base"], &["--base"]), "{given:?}");
        }
    }

    #[test]
    fn run_reports_success_errors_and_findings() {
        assert!(run("seed-check", &[], &workspace_root()).is_ok());
        assert!(run("unknown", &[], Path::new("/missing")).is_err());
        assert!(run("protocol-schema", &[], Path::new("/missing/wo001-cli-test")).is_err());
    }
}
