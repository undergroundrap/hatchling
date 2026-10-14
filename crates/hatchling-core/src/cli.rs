//! Configuration-free help shared by the runtime and development commands.

use std::fmt::Write as _;

/// `EX_USAGE`: an invalid command line, rather than a startup failure.
pub const EX_USAGE: u8 = 64;

/// One environment setting, declared once for both its reader and help.
#[derive(Debug, Clone, Copy)]
pub struct EnvironmentVariable {
    /// The exact environment key the reader uses.
    pub name: &'static str,
    /// Whether it is required, including any conditional requirement.
    pub requirement: &'static str,
    /// Its one-line meaning.
    pub description: &'static str,
}

/// Declares named settings and their complete help inventory together.
///
/// Readers use the generated constant's `name`; adding a declaration
/// automatically adds it to `ENVIRONMENT`, without a second list.
#[macro_export]
macro_rules! declare_environment {
    ($visibility:vis, $($name:ident: ($requirement:literal, $description:literal)),+ $(,)?) => {
        $(
            #[doc = $description]
            $visibility const $name: $crate::cli::EnvironmentVariable = $crate::cli::EnvironmentVariable {
                name: stringify!($name),
                requirement: $requirement,
                description: $description,
            };
        )+
        /// Every setting declared here, in help order.
        $visibility const ENVIRONMENT: &[$crate::cli::EnvironmentVariable] = &[$($name),+];
    };
}

/// A help flag is a complete invocation; extra arguments are usage errors.
#[must_use]
pub fn help_requested(args: &[String]) -> bool {
    matches!(args, [flag] if flag == "--help" || flag == "-h")
}

/// Renders help without reading configuration or accessing the filesystem.
#[must_use]
pub fn render_help(
    program: &str,
    usage: &str,
    details: &str,
    environment: &[EnvironmentVariable],
) -> String {
    let mut text = format!("Usage: {program} {usage}\n\n{details}\n\nEnvironment:\n");
    for variable in environment {
        let _ = writeln!(
            text,
            "  {} ({}): {}",
            variable.name, variable.requirement, variable.description
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    declare_environment! {
        pub(super),
        EXAMPLE_REQUIRED: ("required", "Identity of the process."),
        EXAMPLE_OPTIONAL: ("optional; default 8", "Maximum requests."),
    }

    #[test]
    fn help_flags_do_not_hide_unknown_arguments() {
        for flag in ["--help", "-h"] {
            assert!(help_requested(&[flag.to_owned()]));
            assert!(!help_requested(&[flag.to_owned(), "bogus".to_owned()]));
        }
        assert!(!help_requested(&[]));
        assert!(!help_requested(&["--unknown".to_owned()]));
        assert!(!help_requested(&["run".to_owned(), "--help".to_owned()]));
    }

    #[test]
    fn help_preserves_the_invocation_and_every_declared_setting() {
        assert_eq!(EXAMPLE_REQUIRED.name, "EXAMPLE_REQUIRED");
        assert_eq!(EXAMPLE_OPTIONAL.name, "EXAMPLE_OPTIONAL");
        assert_eq!(
            render_help(
                "example nested",
                "[command]",
                "Runs an example.",
                ENVIRONMENT
            ),
            "Usage: example nested [command]\n\nRuns an example.\n\nEnvironment:\n  EXAMPLE_REQUIRED (required): Identity of the process.\n  EXAMPLE_OPTIONAL (optional; default 8): Maximum requests.\n"
        );
        assert_eq!(
            render_help("empty", "", "No settings.", &[]),
            "Usage: empty \n\nNo settings.\n\nEnvironment:\n"
        );
    }
}
