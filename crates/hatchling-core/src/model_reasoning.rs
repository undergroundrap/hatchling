//! Operator-selected thinking mode for the ordinary model client.
//!
//! This is shared configuration vocabulary, not an egress restriction.

use serde::{Deserialize, Serialize};

/// Thinking behavior requested from an OpenAI-compatible endpoint.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelReasoning {
    /// Leave the request unchanged and let the provider choose.
    #[default]
    ProviderDefault,
    /// Send `reasoning: {"enabled": false}`; an endpoint may refuse it.
    Disabled,
}

impl ModelReasoning {
    /// Reads the exact configuration vocabulary; unknown values fail closed.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "provider-default" => Some(Self::ProviderDefault),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    /// The spelling shared by environment settings and serialized runs.
    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::ProviderDefault => "provider-default",
            Self::Disabled => "disabled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_spellings_parse_serialize_and_round_trip() {
        assert_eq!(ModelReasoning::default(), ModelReasoning::ProviderDefault);
        for (raw, mode) in [
            ("provider-default", ModelReasoning::ProviderDefault),
            ("disabled", ModelReasoning::Disabled),
        ] {
            assert_eq!(ModelReasoning::parse(raw), Some(mode));
            assert_eq!(mode.as_setting(), raw);
            let encoded = serde_json::to_string(&mode).unwrap();
            assert_eq!(encoded, format!("\"{raw}\""));
            assert_eq!(
                serde_json::from_str::<ModelReasoning>(&encoded).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn unknown_or_ambiguous_modes_are_refused() {
        for raw in [
            "",
            "off",
            "false",
            "Disabled",
            "disabled ",
            " disabled",
            "auto",
        ] {
            assert_eq!(ModelReasoning::parse(raw), None, "{raw:?}");
            assert!(serde_json::from_value::<ModelReasoning>(serde_json::json!(raw)).is_err());
        }
        assert!(serde_json::from_str::<ModelReasoning>("null").is_err());
    }
}
