//! What a cell sends, read strictly (WO-027).
//!
//! A frame from a cell is JSON in the narrow form an approval's record can
//! hold: no key twice in one object, compared once unescaped, so a key
//! spelled with a JSON escape is the same key spelled plainly; and no
//! number but an integer
//! within ±[`MAX_SAFE_INTEGER`]. A lone surrogate is refused by the JSON
//! parser itself. A parser that takes the last of two keys, or rounds a
//! number, would read a different request than the one another reader
//! (the owner's app, a log, a later check) sees, so a frame that could be
//! read two ways is refused before it is read at all.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::action::MAX_SAFE_INTEGER;

/// Whether `bytes` is one JSON value, strictly read.
///
/// # Errors
///
/// The parser's error for anything that isn't JSON, and a sentence for a
/// key repeated in one object or a number that isn't an integer within
/// ±[`MAX_SAFE_INTEGER`]. Neither names a value from the frame.
pub fn check(bytes: &[u8]) -> Result<(), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    Strict.deserialize(&mut deserializer)?;
    deserializer.end()
}

/// The strict reading of one value, and of every value in it.
struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without a repeated key or an inexact number")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, number: i64) -> Result<(), E> {
        if number.unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs() {
            Ok(())
        } else {
            Err(E::custom("an integer outside ±(2^53 - 1)"))
        }
    }

    fn visit_u64<E: de::Error>(self, number: u64) -> Result<(), E> {
        if number <= MAX_SAFE_INTEGER.unsigned_abs() {
            Ok(())
        } else {
            Err(E::custom("an integer outside ±(2^53 - 1)"))
        }
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Err(E::custom("a number that isn't an integer"))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(Self)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("a key repeated in one object"));
            }
            map.next_value_seed(Self)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(json: &str) -> String {
        check(json.as_bytes())
            .err()
            .map_or_else(|| format!("{json} was taken"), |error| error.to_string())
    }

    // A JSON escape is written with `\x5c`, the backslash, so each one is
    // in the test as JSON has it, never as the character it stands for.

    #[test]
    fn json_with_each_key_once_and_safe_integers_is_taken() {
        for json in [
            r#"{"kind":"artifact","op":"propose-activation","idea":"idea-1"}"#,
            "{\"a\":{\"a\":1},\"b\":[{\"a\":2},{\"a\":3}],\"c\":null,\"d\":true,\"e\":\"\x5cu00e9\"}",
            "{\"\x5cu006fp\":\"build\",\"kind\":\"artifact\"}",
            "9007199254740991",
            "-9007199254740991",
            "[]",
            "{}",
        ] {
            assert!(check(json.as_bytes()).is_ok(), "{json}");
        }
    }

    #[test]
    fn a_key_twice_in_one_object_is_refused_however_it_is_escaped() {
        for json in [
            r#"{"op":"build","op":"activate"}"#,
            "{\"op\":\"build\",\"\x5cu006fp\":\"activate\"}",
            "{\"\x5cu006f\x5cu0070\":\"build\",\"o\x5cu0070\":\"activate\"}",
            r#"{"outer":{"idea":"a","idea":"b"}}"#,
            "[{\"idea\":\"a\",\"\x5cu0069dea\":\"b\"}]",
        ] {
            let reason = refused(json);
            assert!(reason.contains("repeated"), "{json}: {reason}");
        }
    }

    #[test]
    fn the_strict_reading_of_one_value_reads_all_of_it() {
        let read = |json: &str| Strict.deserialize(&mut serde_json::Deserializer::from_str(json));
        assert!(read(r#"{"a":[1,{"b":true}]}"#).is_ok());
        assert!(read(r#"{"a":[1,{"b":0.5}]}"#).is_err(), "a fraction, deep");
        assert!(read(r#"[{"b":1,"b":2}]"#).is_err(), "a key twice, deep");
    }

    #[test]
    fn a_number_that_isnt_a_safe_integer_is_refused() {
        for json in [
            "1.5",
            "1.0",
            "1e2",
            "9007199254740992",
            "-9007199254740992",
            "18446744073709551616",
            r#"{"revision":2.0}"#,
            r#"{"inputs":[{"size":1e400}]}"#,
        ] {
            assert!(!refused(json).contains("was taken"), "{json}");
        }
    }

    #[test]
    fn a_lone_surrogate_trailing_data_and_broken_json_are_refused() {
        for json in [
            "{\"a\":\"\x5cud800\"}",
            "{\"a\":\"\x5cudc00x\"}",
            "{} {}",
            "{",
            "",
            r#"{"a":1,}"#,
        ] {
            assert!(!refused(json).contains("was taken"), "{json}");
        }
    }
}
