//! CTAP2's CBOR: the canonical subset the protocol uses, and nothing more.
//!
//! A device's reply is untrusted input, so the decoder is strict:
//!
//! - **Only CTAP2's types:** unsigned and negative integers, byte and text
//!   strings, arrays, maps, `false`, `true` and `null`. No tags, no floats,
//!   no other simple values, no indefinite lengths.
//! - **One meaning:** a map holds no key twice, and its keys are integers
//!   or text.
//! - **Bounded:** nesting stops at [`MAX_DEPTH`]; every length is checked
//!   against the bytes left before anything is allocated, so a short reply
//!   cannot claim a huge array.
//!
//! The encoder writes canonical CBOR: shortest forms, and map keys in
//! CTAP2's canonical order (lower major type first, then shorter, then
//! bytewise).
//!
//! The decoder doesn't insist on that order, or on the shortest form of a
//! length or integer, which CTAP2 asks of keys too. A reply means the same
//! either way, as no key can appear twice however it's written, and not
//! every key keeps to them: refusing those would stop no attack, and only
//! stop them working. libfido2 takes them too.

/// How deep arrays and maps may nest. CTAP2's replies need four.
pub const MAX_DEPTH: usize = 8;

/// One CBOR data item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// An unsigned integer.
    Unsigned(u64),
    /// A negative integer: `-1 - n`.
    Negative(u64),
    /// A byte string.
    Bytes(Vec<u8>),
    /// A text string.
    Text(String),
    /// An array.
    Array(Vec<Self>),
    /// A map, in the order it was written or read.
    Map(Vec<(Self, Self)>),
    /// `false` or `true`.
    Bool(bool),
    /// `null`.
    Null,
}

/// Why bytes are not one CTAP2 CBOR item.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CborError {
    /// The bytes ended inside an item.
    #[error("CBOR ends early")]
    Truncated,
    /// Bytes follow the item.
    #[error("bytes follow the CBOR item")]
    Trailing,
    /// A type, length form or simple value CTAP2 does not use.
    #[error("CBOR outside CTAP2's subset: {0}")]
    Unsupported(&'static str),
    /// Arrays and maps nested deeper than [`MAX_DEPTH`].
    #[error("CBOR nested too deep")]
    TooDeep,
    /// A map holds a key twice.
    #[error("CBOR map repeats a key")]
    DuplicateKey,
    /// A map key that isn't an integer or text.
    #[error("CBOR map key of a type CTAP2 doesn't use")]
    KeyType,
    /// A text string that is not UTF-8.
    #[error("CBOR text is not UTF-8")]
    NotUtf8,
}

impl Value {
    /// The integer `n`, as CBOR writes it.
    #[must_use]
    pub fn int(n: i64) -> Self {
        u64::try_from(n).map_or_else(
            |_| Self::Negative(n.unsigned_abs().saturating_sub(1)),
            Self::Unsigned,
        )
    }

    /// A text string.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self::Text(text.to_owned())
    }

    /// A map with its entries in CTAP2's canonical key order.
    #[must_use]
    pub fn map(mut entries: Vec<(Self, Self)>) -> Self {
        entries.sort_by_cached_key(|(key, _)| {
            let bytes = encode(key);
            (bytes.first().map(|head| head >> 5), bytes.len(), bytes)
        });
        Self::Map(entries)
    }

    /// This integer, if it is one that fits an `i64`.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Unsigned(n) => i64::try_from(*n).ok(),
            Self::Negative(n) => i64::try_from(*n).ok().map(|n| -1 - n),
            Self::Bytes(_)
            | Self::Text(_)
            | Self::Array(_)
            | Self::Map(_)
            | Self::Bool(_)
            | Self::Null => None,
        }
    }

    /// The value under `key` in this map.
    #[must_use]
    pub fn get(&self, key: &Self) -> Option<&Self> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find_map(|(k, value)| (k == key).then_some(value)),
            Self::Unsigned(_)
            | Self::Negative(_)
            | Self::Bytes(_)
            | Self::Text(_)
            | Self::Array(_)
            | Self::Bool(_)
            | Self::Null => None,
        }
    }

    /// The value under integer key `key` in this map.
    #[must_use]
    pub fn at(&self, key: i64) -> Option<&Self> {
        self.get(&Self::int(key))
    }

    /// The value under text key `key` in this map.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&Self> {
        self.get(&Self::text(key))
    }

    /// These bytes, if this is a byte string.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Unsigned(_)
            | Self::Negative(_)
            | Self::Text(_)
            | Self::Array(_)
            | Self::Map(_)
            | Self::Bool(_)
            | Self::Null => None,
        }
    }

    /// This boolean, if it is one.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::Unsigned(_)
            | Self::Negative(_)
            | Self::Bytes(_)
            | Self::Text(_)
            | Self::Array(_)
            | Self::Map(_)
            | Self::Null => None,
        }
    }

    /// This array's items, if it is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            Self::Unsigned(_)
            | Self::Negative(_)
            | Self::Bytes(_)
            | Self::Text(_)
            | Self::Map(_)
            | Self::Bool(_)
            | Self::Null => None,
        }
    }
}

/// `value` as canonical CBOR.
#[must_use]
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Unsigned(n) => head(0, *n, out),
        Value::Negative(n) => head(1, *n, out),
        Value::Bytes(bytes) => {
            head(2, length(bytes.len()), out);
            out.extend_from_slice(bytes);
        }
        Value::Text(text) => {
            head(3, length(text.len()), out);
            out.extend_from_slice(text.as_bytes());
        }
        Value::Array(items) => {
            head(4, length(items.len()), out);
            for item in items {
                write(item, out);
            }
        }
        Value::Map(entries) => {
            head(5, length(entries.len()), out);
            for (key, item) in entries {
                write(key, out);
                write(item, out);
            }
        }
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Null => out.push(0xf6),
    }
}

/// A length as CBOR's 64-bit argument (a `usize` always fits).
fn length(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// A major type and its argument, in the shortest form. The major type
/// fills the top three bits and the argument form the low five, so they
/// are added: they never overlap.
fn head(major: u8, n: u64, out: &mut Vec<u8>) {
    let tag = major << 5;
    if let Ok(small) = u8::try_from(n) {
        if small < 24 {
            out.push(tag + small);
        } else {
            out.extend_from_slice(&[tag + 0x18, small]);
        }
    } else if let Ok(short) = u16::try_from(n) {
        out.push(tag + 0x19);
        out.extend_from_slice(&short.to_be_bytes());
    } else if let Ok(word) = u32::try_from(n) {
        out.push(tag + 0x1a);
        out.extend_from_slice(&word.to_be_bytes());
    } else {
        out.push(tag + 0x1b);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

/// The one CBOR item that is all of `bytes`.
///
/// # Errors
///
/// Returns [`CborError`] for anything but exactly one item.
pub fn decode(bytes: &[u8]) -> Result<Value, CborError> {
    let (value, rest) = decode_prefix(bytes)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(CborError::Trailing)
    }
}

/// The CBOR item at the start of `bytes`, and the bytes after it.
///
/// # Errors
///
/// Returns [`CborError`] if no item starts there.
pub fn decode_prefix(bytes: &[u8]) -> Result<(Value, &[u8]), CborError> {
    read(bytes, 0)
}

/// Splits `n` bytes off the front of `bytes`.
fn take(bytes: &[u8], n: u64) -> Result<(&[u8], &[u8]), CborError> {
    let n = usize::try_from(n).map_err(|_| CborError::Truncated)?;
    bytes.split_at_checked(n).ok_or(CborError::Truncated)
}

/// The head at the front of `bytes`: its major type, its argument, and
/// the bytes after it.
fn read_head(bytes: &[u8]) -> Result<(u8, u64, &[u8]), CborError> {
    let (&first, rest) = bytes.split_first().ok_or(CborError::Truncated)?;
    let (major, info) = (first >> 5, first & 0x1f);
    let width = match info {
        0..=23 => return Ok((major, u64::from(info), rest)),
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        31 => return Err(CborError::Unsupported("indefinite length")),
        _ => return Err(CborError::Unsupported("reserved length form")),
    };
    let (arg, rest) = take(rest, width)?;
    Ok((major, be(arg), rest))
}

/// A big-endian unsigned integer of up to eight bytes.
fn be(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |n, byte| (n << 8) + u64::from(*byte))
}

fn read(bytes: &[u8], depth: usize) -> Result<(Value, &[u8]), CborError> {
    let (major, n, rest) = read_head(bytes)?;
    match major {
        0 => Ok((Value::Unsigned(n), rest)),
        1 => Ok((Value::Negative(n), rest)),
        2 => {
            let (body, rest) = take(rest, n)?;
            Ok((Value::Bytes(body.to_vec()), rest))
        }
        3 => {
            let (body, rest) = take(rest, n)?;
            let text = std::str::from_utf8(body).map_err(|_| CborError::NotUtf8)?;
            Ok((Value::Text(text.to_owned()), rest))
        }
        4 | 5 => read_container(major, n, rest, depth),
        7 => match n {
            20 => Ok((Value::Bool(false), rest)),
            21 => Ok((Value::Bool(true), rest)),
            22 => Ok((Value::Null, rest)),
            _ => Err(CborError::Unsupported("simple value or float")),
        },
        _ => Err(CborError::Unsupported("tag")),
    }
}

/// An array (major 4) or map (major 5) of `n` entries.
fn read_container(
    major: u8,
    n: u64,
    mut rest: &[u8],
    depth: usize,
) -> Result<(Value, &[u8]), CborError> {
    if depth >= MAX_DEPTH {
        return Err(CborError::TooDeep);
    }
    // Every item takes at least one byte: a count past the bytes left is
    // a lie, refused before anything is allocated for it.
    let count = usize::try_from(n)
        .ok()
        .filter(|count| *count <= rest.len())
        .ok_or(CborError::Truncated)?;
    if major == 4 {
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let (item, after) = read(rest, depth + 1)?;
            items.push(item);
            rest = after;
        }
        return Ok((Value::Array(items), rest));
    }
    let mut entries: Vec<(Value, Value)> = Vec::with_capacity(count);
    for _ in 0..count {
        let (key, after) = read(rest, depth + 1)?;
        if !matches!(
            key,
            Value::Unsigned(_) | Value::Negative(_) | Value::Text(_)
        ) {
            return Err(CborError::KeyType);
        }
        let (item, after) = read(after, depth + 1)?;
        if entries.iter().any(|(seen, _)| *seen == key) {
            return Err(CborError::DuplicateKey);
        }
        entries.push((key, item));
        rest = after;
    }
    Ok((Value::Map(entries), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A number written longer than it needs reads the same, and a key
    /// written two ways is still one key.
    #[test]
    fn a_longer_form_means_the_same() {
        for wire in ["1817", "190017", "1a00000017", "1b0000000000000017"] {
            assert_eq!(decode(&hex(wire)), Ok(Value::int(23)), "{wire}");
        }
        assert_eq!(
            decode(&hex("5a0000000107")),
            Ok(Value::Bytes(vec![7])),
            "a length"
        );
        assert_eq!(decode(&hex("a20100180100")), Err(CborError::DuplicateKey));
    }

    /// Keys are integers or text; byte strings, arrays, maps, booleans and
    /// null aren't.
    #[test]
    fn a_map_key_is_an_integer_or_text() {
        for key in ["41ff", "80", "a0", "f5", "f6"] {
            let wire = format!("a1{key}00");
            assert_eq!(decode(&hex(&wire)), Err(CborError::KeyType), "{wire}");
        }
        for key in ["01", "20", "6161"] {
            assert!(decode(&hex(&format!("a1{key}00"))).is_ok(), "{key}");
        }
    }

    /// Order means nothing to a reader: any order is taken, as written.
    #[test]
    fn a_map_in_another_order_means_the_same() {
        let reversed = decode(&hex("a202000100")).unwrap();
        assert_eq!(reversed.at(1), Some(&Value::int(0)));
        assert_eq!(reversed.at(2), Some(&Value::int(0)));
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn integers_take_their_shortest_form() {
        // RFC 8949 appendix A.
        for (n, wire) in [
            (0, "00"),
            (23, "17"),
            (24, "1818"),
            (255, "18ff"),
            (256, "190100"),
            (65_535, "19ffff"),
            (65_536, "1a00010000"),
            (4_294_967_295, "1affffffff"),
            (4_294_967_296, "1b0000000100000000"),
            (-1, "20"),
            (-24, "37"),
            (-25, "3818"),
            (-7, "26"),
            (-257, "390100"),
        ] {
            let value = Value::int(n);
            assert_eq!(encode(&value), hex(wire), "{n}");
            assert_eq!(decode(&hex(wire)).unwrap(), value, "{n}");
            assert_eq!(value.as_int(), Some(n));
        }
        assert_eq!(
            encode(&Value::Unsigned(u64::MAX)),
            hex("1bffffffffffffffff")
        );
        assert_eq!(Value::Unsigned(u64::MAX).as_int(), None);
        assert_eq!(Value::Negative(u64::MAX).as_int(), None);
        assert_eq!(
            Value::int(i64::MIN),
            Value::Negative(i64::MAX.unsigned_abs())
        );
        assert_eq!(Value::int(i64::MIN).as_int(), Some(i64::MIN));
    }

    #[test]
    fn strings_arrays_maps_and_simple_values_round_trip() {
        for (value, wire) in [
            (Value::Bytes(vec![1, 2, 3, 4]), "4401020304"),
            (Value::text("IETF"), "6449455446"),
            (Value::text(""), "60"),
            (
                Value::Array(vec![Value::int(1), Value::int(2), Value::int(3)]),
                "83010203",
            ),
            (
                Value::Map(vec![
                    (Value::int(1), Value::int(2)),
                    (Value::int(3), Value::int(4)),
                ]),
                "a201020304",
            ),
            (Value::Bool(false), "f4"),
            (Value::Bool(true), "f5"),
            (Value::Null, "f6"),
        ] {
            assert_eq!(encode(&value), hex(wire));
            assert_eq!(decode(&hex(wire)).unwrap(), value);
        }
        let long = Value::Bytes(vec![7; 300]);
        assert_eq!(decode(&encode(&long)).unwrap(), long);
    }

    #[test]
    fn maps_are_written_in_canonical_key_order() {
        // Lower major types first, then shorter, then bytewise: 1, 2, the
        // longer 24 (0x1818), then -1 (0x20), then "a" (0x6161).
        let map = Value::map(vec![
            (Value::text("a"), Value::Null),
            (Value::int(-1), Value::Null),
            (Value::int(2), Value::Null),
            (Value::int(1), Value::Null),
            (Value::int(24), Value::Null),
        ]);
        let Value::Map(entries) = &map else {
            unreachable!("{map:?}")
        };
        let keys: Vec<Value> = entries.iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(
            keys,
            [
                Value::int(1),
                Value::int(2),
                Value::int(24),
                Value::int(-1),
                Value::text("a")
            ]
        );
    }

    #[test]
    fn lookups_find_their_keys_and_nothing_else() {
        let map = Value::map(vec![
            (Value::int(1), Value::Bytes(vec![9])),
            (Value::text("up"), Value::Bool(true)),
            (Value::int(4), Value::Array(vec![Value::Null])),
        ]);
        assert_eq!(map.at(1).and_then(Value::as_bytes), Some(&[9u8][..]));
        assert_eq!(map.field("up").and_then(Value::as_bool), Some(true));
        assert_eq!(
            map.at(4).and_then(Value::as_array).map(<[Value]>::len),
            Some(1)
        );
        assert_eq!(map.at(2), None);
        assert_eq!(map.field("uv"), None);
        for other in [
            Value::Unsigned(1),
            Value::Negative(0),
            Value::Bytes(vec![]),
            Value::text("x"),
            Value::Array(vec![]),
            Value::Bool(true),
            Value::Null,
        ] {
            assert_eq!(other.at(1), None);
            assert_eq!(other.as_bytes().is_some(), matches!(other, Value::Bytes(_)));
            assert_eq!(other.as_bool().is_some(), matches!(other, Value::Bool(_)));
            assert_eq!(other.as_array().is_some(), matches!(other, Value::Array(_)));
        }
        assert_eq!(Value::text("x").as_int(), None);
        assert_eq!(map.as_int(), None);
    }

    #[test]
    fn a_prefix_leaves_the_bytes_after_it() {
        let mut bytes = encode(&Value::int(500));
        bytes.extend_from_slice(&[0xaa, 0xbb]);
        let (value, rest) = decode_prefix(&bytes).unwrap();
        assert_eq!(value, Value::int(500));
        assert_eq!(rest, [0xaa, 0xbb]);
        assert_eq!(decode(&bytes), Err(CborError::Trailing));
    }

    #[test]
    fn anything_outside_the_subset_is_refused() {
        for (wire, error) in [
            ("", CborError::Truncated),
            ("18", CborError::Truncated),
            ("19ff", CborError::Truncated),
            ("1aff", CborError::Truncated),
            ("1bff", CborError::Truncated),
            ("4401", CborError::Truncated),
            ("6449", CborError::Truncated),
            ("8301", CborError::Truncated),
            ("a101", CborError::Truncated),
            ("5f", CborError::Unsupported("indefinite length")),
            ("9f", CborError::Unsupported("indefinite length")),
            ("1c", CborError::Unsupported("reserved length form")),
            ("1e", CborError::Unsupported("reserved length form")),
            ("c100", CborError::Unsupported("tag")),
            ("f7", CborError::Unsupported("simple value or float")),
            ("f93c00", CborError::Unsupported("simple value or float")),
            ("62c328", CborError::NotUtf8),
            ("a201000100", CborError::DuplicateKey),
        ] {
            assert_eq!(decode(&hex(wire)), Err(error), "{wire}");
        }
    }

    #[test]
    fn a_count_past_the_bytes_left_is_refused_before_allocating() {
        // An array claiming 2^32 items in a few bytes.
        assert_eq!(decode(&hex("9affffffff00")), Err(CborError::Truncated));
        assert_eq!(
            decode(&hex("bbffffffffffffffff00")),
            Err(CborError::Truncated)
        );
        assert_eq!(
            decode(&hex("5bffffffffffffffff")),
            Err(CborError::Truncated)
        );
        // Exactly as many bytes as items is fine.
        assert_eq!(
            decode(&hex("820000")).unwrap(),
            Value::Array(vec![Value::int(0), Value::int(0)])
        );
    }

    #[test]
    fn nesting_stops_at_the_limit() {
        let mut value = Value::Null;
        for _ in 0..MAX_DEPTH {
            value = Value::Array(vec![value]);
        }
        assert!(
            decode(&encode(&value)).is_ok(),
            "{MAX_DEPTH} levels are fine"
        );
        let deeper = Value::Array(vec![value]);
        assert_eq!(decode(&encode(&deeper)), Err(CborError::TooDeep));
        let mut map = Value::Null;
        for _ in 0..=MAX_DEPTH {
            map = Value::Map(vec![(Value::int(1), map)]);
        }
        assert_eq!(decode(&encode(&map)), Err(CborError::TooDeep));
        // Through keys as well: a map whose key is a map, and so on. Within
        // the limit that's a key of the wrong type; past it, too deep.
        let mut keyed = Value::Null;
        for _ in 0..MAX_DEPTH {
            keyed = Value::Map(vec![(keyed, Value::Null)]);
        }
        assert_eq!(decode(&encode(&keyed)), Err(CborError::KeyType));
        let deeper = Value::Map(vec![(keyed, Value::Null)]);
        assert_eq!(decode(&encode(&deeper)), Err(CborError::TooDeep));
    }

    #[test]
    fn the_head_parser_reads_each_length_form() {
        assert_eq!(read_head(&hex("17")).unwrap(), (0, 23, &[][..]));
        assert_eq!(read_head(&hex("1818ff")).unwrap(), (0, 24, &[0xffu8][..]));
        assert_eq!(read_head(&hex("390100")).unwrap(), (1, 256, &[][..]));
        assert_eq!(read_head(&hex("5a00010000")).unwrap(), (2, 65_536, &[][..]));
        assert_eq!(
            read_head(&hex("7b0000000100000000")).unwrap(),
            (3, 4_294_967_296, &[][..])
        );
        assert_eq!(be(&[1, 2]), 0x0102);
        assert_eq!(be(&[]), 0);
        assert_eq!(take(&[1, 2, 3], 2).unwrap(), (&[1u8, 2][..], &[3u8][..]));
        assert_eq!(take(&[1], 2), Err(CborError::Truncated));
        assert_eq!(length(5), 5);
        let mut out = Vec::new();
        head(4, 2, &mut out);
        write(&Value::Null, &mut out);
        assert_eq!(out, hex("82f6"));
        assert_eq!(
            read(&hex("8100"), MAX_DEPTH - 1).unwrap().0,
            Value::Array(vec![Value::int(0)])
        );
        assert_eq!(read(&hex("8100"), MAX_DEPTH), Err(CborError::TooDeep));
        assert_eq!(
            read_container(5, 1, &hex("0102"), 0).unwrap().0,
            Value::Map(vec![(Value::int(1), Value::int(2))])
        );
    }

    #[test]
    fn errors_describe_themselves() {
        assert_eq!(CborError::Truncated.to_string(), "CBOR ends early");
        assert_eq!(
            CborError::Unsupported("tag").to_string(),
            "CBOR outside CTAP2's subset: tag"
        );
    }
}
