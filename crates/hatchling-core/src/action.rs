//! Frozen actions: the canonical form and digest an approval binds
//! (WO-027).
//!
//! Before the owner is asked to approve an action, the broker resolves it
//! into a record of everything its effect depends on, and freezes it. The
//! approval binds that record's digest, and the broker runs that record:
//! nothing a cell sends afterwards is what runs.
//!
//! The canonical form is RFC 8785 (JCS) for the types an action uses:
//! objects, arrays, strings, booleans, and integers within
//! ±[`MAX_SAFE_INTEGER`]. Object keys are sorted by their UTF-16 code
//! units, strings are escaped as JCS escapes them, and nothing is
//! normalized: `"é"` and `"e\u0301"` are different strings, and two
//! actions that differ in them have different digests. A `null`, a
//! fraction, an exponent or an integer out of range has no canonical
//! form here, so it can't be part of an action.
//!
//! The digest is `SHA-256(DOMAIN ‖ JCS(action))`, in lowercase hex: the
//! domain tag keeps it apart from every other hash Hatchling makes.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The domain tag every action digest starts with.
pub const DOMAIN: &[u8] = b"hatchling.action.v1\0";

/// The largest integer an action may hold, and the negation of the
/// smallest: the integers every JSON implementation reads exactly
/// (I-JSON, RFC 7493).
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Why a value has no canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalError {
    /// A number that isn't an integer within ±[`MAX_SAFE_INTEGER`].
    #[error("{0} isn't an integer within ±(2^53 - 1)")]
    Number(String),
    /// A `null`, which no action field takes.
    #[error("an action holds no null")]
    Null,
}

/// The canonical form of `value`, as RFC 8785 writes it.
///
/// # Errors
///
/// [`CanonicalError`] for a value an action can't hold.
pub fn canonical(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

/// The digest an approval binds: SHA-256 over [`DOMAIN`] and the
/// canonical form of `action`, in lowercase hex.
///
/// # Errors
///
/// As [`canonical`].
pub fn digest(action: &Value) -> Result<String, CanonicalError> {
    let form = canonical(action)?;
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(form.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Appends the canonical form of `value` to `out`.
fn write(value: &Value, out: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => return Err(CanonicalError::Null),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            let integer = number
                .as_i64()
                .filter(|n| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(n))
                .ok_or_else(|| CanonicalError::Number(number.to_string()))?;
            let _ = write!(out, "{integer}");
        }
        Value::String(text) => escape(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                escape(key, out);
                out.push(':');
                if let Some(member) = members.get(key) {
                    write(member, out)?;
                }
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Appends `text` as a JSON string, escaped as RFC 8785 escapes it: `"`
/// and `\`, the short forms of five control characters, the rest of
/// U+0000 to U+001F as `\u00xx`, and everything else as itself.
fn escape(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// RFC 8785, section 3.2.3: keys sorted by their UTF-16 code units.
    /// U+1F600 (a surrogate pair, D83D DE00) sorts before U+FB33, where
    /// sorting by UTF-8 bytes or scalar values would put it last.
    #[test]
    fn keys_sort_by_utf16_code_units_as_rfc_8785_sorts_them() {
        let raw = r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#;
        let value: Value = serde_json::from_str(raw).unwrap();
        let form = canonical(&value).unwrap();
        let order: Vec<&str> = [
            "Carriage Return",
            "One",
            "Control",
            "Latin Small Letter O With Diaeresis",
            "Euro Sign",
            "Emoji: Grinning Face",
            "Hebrew Letter Dalet With Dagesh",
        ]
        .to_vec();
        let positions: Vec<usize> = order.iter().map(|name| form.find(name).unwrap()).collect();
        assert!(
            positions
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if a < b)),
            "{form}"
        );
        assert!(form.starts_with("{\"\\r\":"), "{form}");
    }

    /// RFC 8785's string escaping, and no normalization.
    #[test]
    fn strings_escape_as_jcs_escapes_them_and_are_never_normalized() {
        let value = json!({
            "s": "\u{20ac}$\u{f}\nA'B\"\\\\\"/",
            "t": "\u{8}\t\u{c}\r\u{1f}\u{7f}",
        });
        assert_eq!(
            canonical(&value).unwrap(),
            r#"{"s":"€$\u000f\nA'B\"\\\\\"/","t":"\b\t\f\r\u001f"#.to_owned() + "\u{7f}\"}"
        );
        let composed = digest(&json!({ "name": "\u{e9}" })).unwrap();
        let decomposed = digest(&json!({ "name": "e\u{301}" })).unwrap();
        assert_ne!(composed, decomposed, "no normalization");
    }

    #[test]
    fn only_safe_integers_booleans_strings_arrays_and_objects_have_a_form() {
        assert_eq!(
            canonical(&json!([
                true,
                false,
                0,
                -1,
                MAX_SAFE_INTEGER,
                -MAX_SAFE_INTEGER,
                [],
                {}
            ]))
            .unwrap(),
            "[true,false,0,-1,9007199254740991,-9007199254740991,[],{}]"
        );
        for refused in [
            json!(null),
            json!({ "a": null }),
            json!(1.5),
            json!(1.0),
            json!(MAX_SAFE_INTEGER + 1),
            json!(-MAX_SAFE_INTEGER - 1),
            json!(u64::MAX),
            json!([1, 2.5]),
        ] {
            assert!(canonical(&refused).is_err(), "{refused}");
        }
        assert_eq!(canonical(&json!(null)), Err(CanonicalError::Null));
        assert!(matches!(canonical(&json!(0.5)), Err(CanonicalError::Number(n)) if n == "0.5"));
    }

    /// A lone surrogate never becomes a string an action could hold:
    /// JSON with one is refused before it is a value.
    #[test]
    fn a_lone_surrogate_is_refused_before_it_is_a_value() {
        for raw in [r#""\ud800""#, r#""\udc00""#, r#""\ud83dx""#] {
            assert!(serde_json::from_str::<Value>(raw).is_err(), "{raw}");
        }
        assert_eq!(
            serde_json::from_str::<Value>(r#""\ud83d\ude00""#).unwrap(),
            json!("\u{1f600}")
        );
    }

    /// The digest: field order and whitespace don't change it, any field
    /// does, and the domain tag is part of it.
    #[test]
    fn the_digest_changes_exactly_when_the_action_does() {
        let action = json!({
            "op": "activate",
            "executor": { "name": "artifact-builder", "contract": 1 },
            "idea": { "id": "idea-1", "revision": 3 },
            "inputs": [{ "name": "spec", "sha256": "ab", "len": 12 }],
        });
        let reordered: Value = serde_json::from_str(
            r#"{ "inputs" : [ {"len":12, "sha256":"ab","name":"spec"} ],
                 "idea": {"revision":3,"id":"idea-1"},
                 "executor": {"contract":1,"name":"artifact-builder"}, "op":"activate" }"#,
        )
        .unwrap();
        let base = digest(&action).unwrap();
        assert_eq!(base.len(), 64);
        assert!(
            base.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(digest(&reordered).unwrap(), base);
        let changes = [
            json!({"op": "activate!"}),
            json!({"executor": { "name": "artifact-builder", "contract": 2 }}),
            json!({"idea": { "id": "idea-1", "revision": 4 }}),
            json!({"inputs": [{ "name": "spec", "sha256": "ab", "len": 13 }]}),
            json!({"inputs": []}),
            json!({"extra": true}),
        ];
        for change in changes {
            let mut altered = action.clone();
            if let (Some(target), Some(fields)) = (altered.as_object_mut(), change.as_object()) {
                for (key, value) in fields {
                    target.insert(key.clone(), value.clone());
                }
            }
            assert_ne!(digest(&altered).unwrap(), base, "{change}");
        }
        // The domain tag is hashed: a SHA-256 of the form alone differs.
        let untagged = Sha256::digest(canonical(&action).unwrap().as_bytes())
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_ne!(untagged, base);
        assert!(digest(&json!({ "a": null })).is_err());
    }
}
