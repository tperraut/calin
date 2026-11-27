//! Reading JSON that comes from outside.

use serde::de::DeserializeOwned;

use crate::secret::log_safe;

/// Deserializes `bytes` into `T` only if they hold a JSON object.
///
/// A derived `Deserialize` also accepts an array, reading the fields by
/// position: `["none"]` would pass for a header. Every document this crate
/// reads is an object, so anything else is refused here. Typed
/// deserialization is kept (rather than going through a generic map)
/// because it refuses a member that appears twice. The error is safe to log.
pub(crate) fn parse_object<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let first = bytes.iter().find(|byte| !b" \t\r\n".contains(byte));
    if first != Some(&b'{') {
        return Err("not a JSON object".to_owned());
    }
    serde_json::from_slice(bytes).map_err(|error| log_safe(&error.to_string()))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Sample {
        name: String,
        count: Option<u32>,
    }

    #[test]
    fn objects_are_read() {
        let expected = Sample {
            name: "a".to_owned(),
            count: Some(2),
        };
        assert_eq!(
            parse_object::<Sample>(br#"{"name":"a","count":2}"#),
            Ok(expected)
        );
        assert!(parse_object::<Sample>(b" \t\r\n{\"name\":\"a\",\"other\":[1]}").is_ok());
    }

    #[test]
    fn anything_but_an_object_is_refused() {
        // serde alone would accept the first of these.
        assert!(serde_json::from_slice::<Sample>(br#"["a", 2]"#).is_ok());
        for body in [
            r#"["a", 2]"#,
            "",
            "  ",
            "null",
            "\"a\"",
            "7",
            "true",
            "\u{feff}{}",
            "<html>",
        ] {
            assert_eq!(
                parse_object::<Sample>(body.as_bytes()),
                Err("not a JSON object".to_owned()),
                "{body:?}"
            );
        }
    }

    #[test]
    fn errors_are_one_printable_line() {
        let error = parse_object::<Sample>(b"{\"name\": \"a\nb\"}").unwrap_err();
        assert!(
            error.chars().all(|c| c.is_ascii_graphic() || c == ' '),
            "{error:?}"
        );
        let error = parse_object::<Sample>(br#"{"name":"a","name":"b"}"#).unwrap_err();
        assert!(error.starts_with("duplicate field `name`"), "{error}");
        let error = parse_object::<Sample>(b"{}").unwrap_err();
        assert!(error.starts_with("missing field `name`"), "{error}");
    }
}
