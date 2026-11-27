//! A YAML value tree that keeps the line of every node.
//!
//! The file is first read into this generic tree, then converted to the typed model by hand.
//! Going through a tree instead of `#[derive(Deserialize)]` on the model is what lets parsing
//! report every problem in one pass, each with its line: serde stops at the first error.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_saphyr::Spanned;

/// A YAML node and the 1-based line it starts on, when the parser knows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Raw {
    pub line: Option<u32>,
    pub value: RawValue,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawValue {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Seq(Vec<Raw>),
    /// Entries in file order. Keys are kept as nodes so a non-string key can be reported.
    Map(Vec<(Raw, Raw)>),
}

impl RawValue {
    /// The YAML kind, for "expected X, found Y" messages.
    pub fn kind(&self) -> &'static str {
        match self {
            RawValue::Null => "null",
            RawValue::Bool(_) => "a boolean",
            RawValue::Int(_) => "an integer",
            RawValue::Float(_) => "a number",
            RawValue::Str(_) => "a string",
            RawValue::Seq(_) => "a list",
            RawValue::Map(_) => "a mapping",
        }
    }
}

impl<'de> Deserialize<'de> for Raw {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spanned = Spanned::<RawValue>::deserialize(deserializer)?;
        let line = spanned.referenced.line();
        Ok(Raw {
            // Line 0 is the parser's "unknown".
            line: u32::try_from(line).ok().filter(|l| *l > 0),
            value: spanned.value,
        })
    }
}

impl<'de> Deserialize<'de> for RawValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RawVisitor)
    }
}

struct RawVisitor;

impl<'de> Visitor<'de> for RawVisitor {
    type Value = RawValue;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<RawValue, E> {
        Ok(RawValue::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<RawValue, E> {
        Ok(RawValue::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<RawValue, D::Error> {
        RawValue::deserialize(d)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<RawValue, E> {
        Ok(RawValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<RawValue, E> {
        Ok(RawValue::Int(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<RawValue, E> {
        Ok(RawValue::Int(v.into()))
    }

    fn visit_i128<E: de::Error>(self, v: i128) -> Result<RawValue, E> {
        Ok(RawValue::Int(v))
    }

    fn visit_u128<E: de::Error>(self, v: u128) -> Result<RawValue, E> {
        // Anything this large is out of every range the format allows anyway.
        Ok(RawValue::Int(i128::try_from(v).unwrap_or(i128::MAX)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<RawValue, E> {
        Ok(RawValue::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<RawValue, E> {
        Ok(RawValue::Str(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<RawValue, E> {
        Ok(RawValue::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<RawValue, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Raw>()? {
            items.push(item);
        }
        Ok(RawValue::Seq(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawValue, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<Raw>()? {
            let value = map.next_value::<Raw>()?;
            entries.push((key, value));
        }
        Ok(RawValue::Map(entries))
    }
}

/// Reads a whole document. On a YAML error, returns its message and line.
pub(crate) fn read(yaml: &str) -> Result<Raw, (String, Option<u32>)> {
    // YAML 1.2 booleans only, so a verdict or option spelled `yes`/`no` stays a word.
    let options = serde_saphyr::options! { strict_booleans: true };
    serde_saphyr::from_str_with_options::<Raw>(yaml, options).map_err(|e| {
        let line = e
            .location()
            .and_then(|l| u32::try_from(l.line()).ok())
            .filter(|l| *l > 0);
        let text = e.without_snippet().to_string();
        // The parser's own text starts with "error: line L column C: "; the line is kept
        // separately, so drop that prefix to avoid saying it twice.
        let text = text.strip_prefix("error: ").unwrap_or(&text);
        let text = match text.split_once(": ") {
            Some((head, rest)) if head.starts_with("line ") => rest,
            _ => text,
        };
        (format!("invalid YAML: {}", text.trim()), line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_lines() {
        let raw = read("a: 1\nb:\n  - { x: yes, y: 2.5 }\n  - null\n").unwrap();
        let RawValue::Map(entries) = raw.value else {
            panic!("not a map: {raw:?}")
        };
        assert_eq!(entries[0].0.line, Some(1));
        assert_eq!(entries[1].1.line, Some(3));
        let RawValue::Seq(items) = &entries[1].1.value else {
            panic!()
        };
        assert_eq!(items[1].line, Some(4));
        assert_eq!(items[1].value, RawValue::Null);
        let RawValue::Map(inner) = &items[0].value else {
            panic!()
        };
        // YAML 1.2: `yes` is a string.
        assert_eq!(inner[0].1.value, RawValue::Str("yes".into()));
        assert_eq!(inner[1].1.value, RawValue::Float(2.5));
        assert_eq!(inner[1].1.line, Some(3));
    }

    #[test]
    fn syntax_error_has_line() {
        let (msg, line) = read("a: 1\nb: [1, 2\n").unwrap_err();
        assert!(msg.starts_with("invalid YAML"), "{msg}");
        assert!(line.is_some());
    }

    #[test]
    fn duplicate_key_is_error() {
        let (msg, line) = read("a: 1\na: 2\n").unwrap_err();
        assert!(msg.contains("duplicate"), "{msg}");
        assert_eq!(line, Some(2));
    }
}
