//! systemd's `EnvironmentFile=` syntax: writing values so they read back
//! exactly, and reading a file the way systemd does (`load_env_file` in
//! systemd's `src/basic/env-file.c`), which `check` uses and the tests use to
//! prove the round trip.

/// Characters that never need quoting in a value.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '_' | '-' | '|'
        )
}

/// The characters systemd unescapes inside double quotes (`SHELL_NEED_ESCAPE`).
/// A backslash before any other character is kept as is.
const DOUBLE_QUOTE_ESCAPED: [char; 4] = ['"', '\\', '`', '$'];

/// One `KEY=value` line. Values that are not plain are double-quoted with
/// `"`, `\`, `` ` `` and `$` escaped. A value must not contain control
/// characters: validation refuses them before rendering.
pub(crate) fn assignment(key: &str, value: &str) -> String {
    if !value.is_empty() && value.chars().all(is_plain) {
        return format!("{key}={value}");
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for c in value.chars() {
        if DOUBLE_QUOTE_ESCAPED.contains(&c) {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    format!("{key}={quoted}")
}

/// Why a file could not be read. Carries a line number, never content, so it
/// can be printed for a file holding a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {problem}")]
pub struct EnvParseError {
    pub line: usize,
    pub problem: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PreKey,
    Key,
    PreValue,
    Value,
    ValueEscape,
    SingleQuote,
    DoubleQuote,
    DoubleQuoteEscape,
    Comment,
    CommentEscape,
}

/// Reads an environment file as systemd does, in order, later assignments
/// of a key replacing earlier ones. Stricter than systemd in two ways: an
/// unterminated quote and an invalid key are errors rather than ignored.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>, EnvParseError> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut state = State::PreKey;
    let mut key = String::new();
    let mut value = String::new();
    // Length of `value` up to its last character that is not unquoted,
    // unescaped trailing whitespace, which systemd trims.
    let mut value_keep = 0usize;
    let mut line = 1usize;
    let mut key_line = 1usize;

    let commit = |key: &mut String,
                  value: &mut String,
                  keep: usize,
                  pairs: &mut Vec<(String, String)>,
                  at: usize|
     -> Result<(), EnvParseError> {
        let name = key.trim_end().to_owned();
        if !valid_key(&name) {
            return Err(EnvParseError {
                line: at,
                problem: "invalid variable name",
            });
        }
        value.truncate(keep);
        pairs.retain(|(existing, _)| existing != &name);
        pairs.push((name, std::mem::take(value)));
        key.clear();
        Ok(())
    };

    for c in text.chars() {
        let newline = c == '\n' || c == '\r';
        match state {
            State::PreKey => {
                if c == '#' || c == ';' {
                    state = State::Comment;
                } else if !c.is_ascii_whitespace() {
                    key_line = line;
                    key.push(c);
                    state = State::Key;
                }
            }
            State::Key => {
                if newline {
                    return Err(EnvParseError {
                        line,
                        problem: "a line without `=`",
                    });
                } else if c == '=' {
                    value.clear();
                    value_keep = 0;
                    state = State::PreValue;
                } else {
                    key.push(c);
                }
            }
            State::PreValue => {
                if newline {
                    commit(&mut key, &mut value, value_keep, &mut pairs, key_line)?;
                    state = State::PreKey;
                } else if c == '\'' {
                    state = State::SingleQuote;
                } else if c == '"' {
                    state = State::DoubleQuote;
                } else if c == '\\' {
                    state = State::ValueEscape;
                } else if !(c == ' ' || c == '\t') {
                    value.push(c);
                    value_keep = value.len();
                    state = State::Value;
                }
            }
            State::Value => {
                if newline {
                    commit(&mut key, &mut value, value_keep, &mut pairs, key_line)?;
                    state = State::PreKey;
                } else if c == '\\' {
                    state = State::ValueEscape;
                } else {
                    value.push(c);
                    if !(c == ' ' || c == '\t') {
                        value_keep = value.len();
                    }
                }
            }
            State::ValueEscape => {
                state = State::Value;
                if c != '\n' {
                    value.push(c);
                    value_keep = value.len();
                }
            }
            State::SingleQuote => {
                if c == '\'' {
                    state = State::PreValue;
                } else {
                    value.push(c);
                    value_keep = value.len();
                }
            }
            State::DoubleQuote => {
                if c == '"' {
                    state = State::PreValue;
                } else if c == '\\' {
                    state = State::DoubleQuoteEscape;
                } else {
                    value.push(c);
                    value_keep = value.len();
                }
            }
            State::DoubleQuoteEscape => {
                state = State::DoubleQuote;
                if DOUBLE_QUOTE_ESCAPED.contains(&c) {
                    value.push(c);
                } else if c != '\n' {
                    value.push('\\');
                    value.push(c);
                }
                value_keep = value.len();
            }
            State::Comment => {
                if c == '\\' {
                    state = State::CommentEscape;
                } else if newline {
                    state = State::PreKey;
                }
            }
            State::CommentEscape => state = State::Comment,
        }
        if c == '\n' {
            line += 1;
        }
    }

    match state {
        State::PreValue | State::Value | State::ValueEscape => {
            commit(&mut key, &mut value, value_keep, &mut pairs, key_line)?;
        }
        State::SingleQuote | State::DoubleQuote | State::DoubleQuoteEscape => {
            return Err(EnvParseError {
                line: key_line,
                problem: "an unterminated quote",
            });
        }
        State::Key => {
            return Err(EnvParseError {
                line,
                problem: "a line without `=`",
            });
        }
        State::PreKey | State::Comment | State::CommentEscape => {}
    }
    Ok(pairs)
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: [&str; 22] = [
        "plain",
        "",
        "with space",
        "  leading and trailing  ",
        "double \"quotes\"",
        "single 'quotes'",
        "both ' and \"",
        "# not a comment",
        "; not a comment either",
        "dollar $HOME and ${HOME}",
        "$",
        "back\\slash",
        "trailing backslash\\",
        "\\\\double",
        "\\n is not a newline",
        "back`tick`",
        "tab\tinside",
        "equals=in=value",
        "a@example.com,sub:auth0|123",
        "unicode é ✓",
        "p@ss w0rd \"$'\\#",
        "'",
    ];

    #[test]
    fn hostile_values_round_trip() {
        for value in HOSTILE {
            let line = assignment("CALIN_TEST", value);
            let parsed = parse_env_file(&format!("{line}\n")).unwrap();
            assert_eq!(
                parsed,
                [("CALIN_TEST".to_owned(), value.to_owned())],
                "{line}"
            );
            // Also without a final newline, and among other lines.
            let parsed = parse_env_file(&line).unwrap();
            assert_eq!(parsed[0].1, value, "{line}");
            let text = format!("# header\nA=1\n{line}\nB=\"2\"\n");
            let parsed = parse_env_file(&text).unwrap();
            assert_eq!(parsed[1], ("CALIN_TEST".to_owned(), value.to_owned()));
            assert_eq!(parsed[2].1, "2");
        }
    }

    #[test]
    fn rendered_lines_stay_on_one_line() {
        for value in HOSTILE {
            assert_eq!(assignment("K", value).lines().count(), 1, "{value:?}");
        }
    }

    #[test]
    fn the_parser_follows_systemd() {
        let text = "\
# comment \\
continued comment
; another comment
  INDENTED = spaced value   \n\
SINGLE='a \"b\" \\c'\n\
DOUBLE=\"a \\x \\$ \\\\\"\n\
CONCAT='a'\"b\"c\n\
ESCAPED=a\\ \\#b\\\\\n\
EMPTY=\n\
REPEATED=1\n\
REPEATED=2\n";
        let parsed = parse_env_file(text).unwrap();
        let get = |key: &str| {
            parsed
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("INDENTED"), Some("spaced value"));
        assert_eq!(get("SINGLE"), Some("a \"b\" \\c"));
        assert_eq!(get("DOUBLE"), Some("a \\x $ \\"));
        assert_eq!(get("CONCAT"), Some("abc"));
        assert_eq!(get("ESCAPED"), Some("a #b\\"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(get("REPEATED"), Some("2"));
        assert_eq!(parsed.len(), 7);
    }

    #[test]
    fn broken_files_are_errors_without_content() {
        for (text, line) in [
            ("A=1\nB=\"secret\n", 2),
            ("A=1\nnot an assignment\n", 2),
            ("1A=x\n", 1),
            ("A=1\nB='x", 2),
        ] {
            let error = parse_env_file(text).unwrap_err();
            assert_eq!(error.line, line, "{text:?}");
            assert!(!error.to_string().contains("secret"));
        }
    }
}
