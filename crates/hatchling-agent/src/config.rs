//! Agent configuration, parsed once at startup.
//!
//! Invariant: a `Config` value is fully validated. Everything downstream may
//! assume validity without re-checking. Untrusted input (environment
//! variables) becomes validated types here, at the edge, exactly once.

use hatchling_core::{CellId, LogLevel, ModelReasoning, PeerUid, SocketPath};

use crate::Error;
use crate::environment as env;

/// Validated agent configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Identity of the cell this agent runs in.
    pub cell_id: CellId,
    /// Broker socket the agent talks to.
    pub broker_socket: SocketPath,
    /// Expected uid of the broker process, from the kernel's point of
    /// view. The peer is authorized against this value via
    /// `SO_PEERCRED`; there is no other identity signal.
    pub broker_uid: PeerUid,
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Model endpoint URL (`HATCHLING_MODEL_ENDPOINT`), e.g.
    /// `https://models.example.com/v1`. Required for `run`; anything
    /// else in the binary does not need it.
    pub model_endpoint: Option<String>,
    /// Provider-side model name (`HATCHLING_MODEL_NAME`). Required for
    /// `run`.
    pub model_name: Option<String>,
    /// Thinking mode (`HATCHLING_MODEL_REASONING`); unset leaves the provider default.
    pub model_reasoning: ModelReasoning,
    /// Loop step budget for `run` (`HATCHLING_MODEL_MAX_STEPS`).
    pub model_max_steps: u32,
    /// The model's context window in tokens
    /// (`HATCHLING_MODEL_CONTEXT_WINDOW`), if the operator declares it
    /// . At least [`hatchling_model::MIN_CONTEXT_WINDOW`].
    pub model_context_window: Option<u32>,
    /// Whether this agent runs as a subagent (`HATCHLING_RUN_SUBAGENT`,
    /// ): set by the broker for a child's run, which then reports
    /// how it ended in a typed envelope.
    pub run_subagent: bool,
}

impl Config {
    /// Reads and validates configuration from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if any required value is missing or invalid.
    pub fn from_env() -> Result<Self, Error> {
        Self::parse(|key| match std::env::var(key) {
            Ok(value) => Some(value),
            // A present malformed OS value is invalid, not an absent setting.
            Err(std::env::VarError::NotUnicode(_))
                if key == env::HATCHLING_MODEL_REASONING.name =>
            {
                Some(String::new())
            }
            Err(_) => None,
        })
    }

    /// Parses configuration using a key lookup function.
    ///
    /// Split out so tests can inject values without touching process
    /// environment (tests run in parallel; the process env is shared).
    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let cell_id = get(env::HATCHLING_CELL_ID.name)
            .ok_or(Error::MissingConfig("HATCHLING_CELL_ID"))?
            .parse::<CellId>()?;
        let broker_socket = get(env::HATCHLING_BROKER_SOCKET.name)
            .ok_or(Error::MissingConfig("HATCHLING_BROKER_SOCKET"))?
            .parse::<SocketPath>()?;
        let broker_uid = get(env::HATCHLING_BROKER_UID.name)
            .ok_or(Error::MissingConfig("HATCHLING_BROKER_UID"))?
            .parse::<u32>()
            .map(PeerUid::from_kernel)
            .map_err(|_| Error::InvalidConfig {
                key: "HATCHLING_BROKER_UID",
                reason: String::from("expected a kernel uid as a base-10 u32"),
            })?;
        let log_level = match get(env::HATCHLING_LOG_LEVEL.name) {
            Some(raw) => raw.parse::<LogLevel>()?,
            None => LogLevel::Info,
        };
        let model_reasoning = match get(env::HATCHLING_MODEL_REASONING.name) {
            None => ModelReasoning::ProviderDefault,
            Some(raw) => ModelReasoning::parse(&raw).ok_or_else(|| Error::InvalidConfig {
                key: "HATCHLING_MODEL_REASONING",
                reason: String::from("expected provider-default or disabled"),
            })?,
        };
        let model_max_steps = match get(env::HATCHLING_MODEL_MAX_STEPS.name) {
            Some(raw) => raw.parse::<u32>().map_err(|_| Error::InvalidConfig {
                key: "HATCHLING_MODEL_MAX_STEPS",
                reason: String::from("expected a base-10 u32 step budget"),
            })?,
            None => 10,
        };
        let model_context_window = get(env::HATCHLING_MODEL_CONTEXT_WINDOW.name)
            .map(|raw| parse_context_window(&raw))
            .transpose()?;
        let run_subagent = match get(env::HATCHLING_RUN_SUBAGENT.name).as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_RUN_SUBAGENT",
                    reason: String::from("expected 0 or 1"),
                });
            }
        };
        Ok(Self {
            cell_id,
            broker_socket,
            broker_uid,
            log_level,
            model_endpoint: get(env::HATCHLING_MODEL_ENDPOINT.name),
            model_name: get(env::HATCHLING_MODEL_NAME.name),
            model_reasoning,
            model_max_steps,
            model_context_window,
            run_subagent,
        })
    }
}

/// A declared context window: a base-10 token count, no smaller than
/// the loop needs. Refused here, at load, not when a task starts.
fn parse_context_window(raw: &str) -> Result<u32, Error> {
    let key = "HATCHLING_MODEL_CONTEXT_WINDOW";
    let window = raw.parse::<u32>().map_err(|_| Error::InvalidConfig {
        key,
        reason: String::from("expected a base-10 u32 token count"),
    })?;
    hatchling_model::check_context_window(Some(window)).map_err(|error| Error::InvalidConfig {
        key,
        reason: error.to_string(),
    })?;
    Ok(window)
}

#[cfg(test)]
mod tests {

    #[test]
    fn thinking_mode_refuses_nonunicode_environment_before_startup() {
        use std::os::unix::ffi::OsStringExt as _;
        const CHILD: &str = "HATCHLING_TEST_REASONING_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let error = Config::from_env().unwrap_err();
            assert!(
                matches!(error, Error::InvalidConfig { key: "HATCHLING_MODEL_REASONING", reason } if reason == "expected provider-default or disabled")
            );
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::thinking_mode_refuses_nonunicode_environment_before_startup",
                "--nocapture",
            ])
            .env_clear()
            .envs(valid_vars())
            .env(CHILD, "1")
            .env(
                "HATCHLING_MODEL_REASONING",
                std::ffi::OsString::from_vec(vec![0xff]),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    fn another_settings_nonunicode_value_still_reads_as_unset() {
        // Only the thinking mode refuses a non-UTF-8 value; any other
        // setting's reads as unset, as before WO-020. An unset step budget
        // is the default, and an empty one would be refused.
        use std::os::unix::ffi::OsStringExt as _;
        const CHILD: &str = "HATCHLING_TEST_OTHER_NONUNICODE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let config = Config::from_env().unwrap();
            assert_eq!(config.model_max_steps, 10);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::another_settings_nonunicode_value_still_reads_as_unset",
                "--nocapture",
            ])
            .env_clear()
            .envs(valid_vars())
            .env(CHILD, "1")
            .env(
                "HATCHLING_MODEL_MAX_STEPS",
                std::ffi::OsString::from_vec(vec![0xff]),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    use super::*;
    use std::collections::HashMap;

    /// Builds a lookup closure over a map, so tests never touch process env.
    fn lookup<'a>(vars: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| vars.get(key).map(|value| (*value).to_owned())
    }

    fn valid_vars() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("HATCHLING_CELL_ID", "cell-01"),
            ("HATCHLING_BROKER_SOCKET", "/tmp/hatchling-test/broker.sock"),
            ("HATCHLING_BROKER_UID", "1000"),
        ])
    }

    #[test]
    fn parses_valid_config() {
        let vars = valid_vars();
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.cell_id.as_str(), "cell-01");
        assert_eq!(
            config.broker_socket.as_path(),
            std::path::Path::new("/tmp/hatchling-test/broker.sock")
        );
        assert_eq!(config.log_level, LogLevel::Info);
    }

    #[test]
    fn missing_cell_id_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_CELL_ID");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_CELL_ID")));
    }

    #[test]
    fn missing_broker_socket_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_BROKER_SOCKET");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(
            err,
            Error::MissingConfig("HATCHLING_BROKER_SOCKET")
        ));
    }

    #[test]
    fn invalid_cell_id_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_CELL_ID", "not a valid id!");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::InvalidCellId(_)));
    }

    #[test]
    fn relative_socket_path_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_BROKER_SOCKET", "relative/broker.sock");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::InvalidSocketPath(_)));
    }

    #[test]
    fn missing_broker_uid_is_an_error() {
        let mut vars = valid_vars();
        vars.remove("HATCHLING_BROKER_UID");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::MissingConfig("HATCHLING_BROKER_UID")));
    }

    #[test]
    fn invalid_broker_uid_is_an_error() {
        for raw in ["not-a-uid", "-1", ""] {
            let mut vars = valid_vars();
            vars.insert("HATCHLING_BROKER_UID", raw);
            let err = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key, .. } if key == "HATCHLING_BROKER_UID"),
                "raw value {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn valid_broker_uid_parses() {
        let vars = valid_vars();
        let config = Config::parse(lookup(&vars)).unwrap();
        assert_eq!(config.broker_uid, PeerUid::from_kernel(1000));
    }
    #[test]
    fn unknown_log_level_is_an_error() {
        let mut vars = valid_vars();
        vars.insert("HATCHLING_LOG_LEVEL", "verbose");
        let err = Config::parse(lookup(&vars)).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    #[test]
    fn log_levels_parse_case_insensitively() {
        for (raw, expected) in [
            ("error", LogLevel::Error),
            ("WARN", LogLevel::Warn),
            ("Info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("TRACE", LogLevel::Trace),
        ] {
            assert_eq!(raw.parse::<LogLevel>().unwrap(), expected);
        }
    }
    #[test]
    fn a_declared_context_window_is_read_and_held_to_the_floor() {
        let mut vars = valid_vars();
        assert_eq!(
            Config::parse(lookup(&vars)).unwrap().model_context_window,
            None
        );
        for (raw, window) in [("8192", 8192), ("131072", 131_072)] {
            vars.insert("HATCHLING_MODEL_CONTEXT_WINDOW", raw);
            assert_eq!(
                Config::parse(lookup(&vars)).unwrap().model_context_window,
                Some(window)
            );
        }
        for (raw, reason) in [
            (
                "8191",
                "the model cannot do what the loop needs: its declared context window of 8191 tokens is below the 8192 the loop needs",
            ),
            ("", "expected a base-10 u32 token count"),
            ("lots", "expected a base-10 u32 token count"),
            ("-1", "expected a base-10 u32 token count"),
        ] {
            vars.insert("HATCHLING_MODEL_CONTEXT_WINDOW", raw);
            let error = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(&error, Error::InvalidConfig { key, reason: got }
                    if *key == "HATCHLING_MODEL_CONTEXT_WINDOW" && got == reason),
                "{raw:?}: {error:?}"
            );
        }
    }

    #[test]
    fn the_subagent_mode_is_read_strictly() {
        let mut vars = valid_vars();
        assert!(!Config::parse(lookup(&vars)).unwrap().run_subagent);
        for (raw, expected) in [("0", false), ("1", true)] {
            vars.insert("HATCHLING_RUN_SUBAGENT", raw);
            assert_eq!(Config::parse(lookup(&vars)).unwrap().run_subagent, expected);
        }
        for raw in ["", "yes", "2", "true"] {
            vars.insert("HATCHLING_RUN_SUBAGENT", raw);
            let error = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(&error, Error::InvalidConfig { key, reason }
                    if *key == "HATCHLING_RUN_SUBAGENT" && reason == "expected 0 or 1"),
                "{raw:?}: {error:?}"
            );
        }
    }

    #[test]
    fn thinking_mode_is_strict_and_defaults_without_an_extension() {
        let mut vars = valid_vars();
        assert_eq!(
            Config::parse(lookup(&vars)).unwrap().model_reasoning,
            ModelReasoning::ProviderDefault
        );
        for (raw, expected) in [
            ("provider-default", ModelReasoning::ProviderDefault),
            ("disabled", ModelReasoning::Disabled),
        ] {
            vars.insert("HATCHLING_MODEL_REASONING", raw);
            assert_eq!(
                Config::parse(lookup(&vars)).unwrap().model_reasoning,
                expected
            );
        }
        for raw in ["", "off", "false", "Disabled", "disabled ", "auto"] {
            vars.insert("HATCHLING_MODEL_REASONING", raw);
            let error = Config::parse(lookup(&vars)).unwrap_err();
            assert!(
                matches!(error, Error::InvalidConfig { key: "HATCHLING_MODEL_REASONING", reason } if reason == "expected provider-default or disabled")
            );
        }
    }
}
