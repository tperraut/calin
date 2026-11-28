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

/// Where the reader is; `escaped` is true right after a backslash.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PreKey,
    Key,
    PreValue,
    Value { escaped: bool },
    SingleQuote,
    DoubleQuote { escaped: bool },
    Comment { escaped: bool },
}

impl State {
    /// The states a line break ends; in the others it is part of a quoted
    /// value, escaped, or blank space before a key.
    fn ends_at_line_break(self) -> bool {
        matches!(
            self,
            State::Key
                | State::PreValue
                | State::Value { escaped: false }
                | State::Comment { escaped: false }
        )
    }
}

fn is_line_break(c: char) -> bool {
    c == '\n' || c == '\r'
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Reads an environment file as systemd does, in order, later assignments
/// of a key replacing earlier ones. Stricter than systemd in two ways: an
/// unterminated quote and an invalid key are errors rather than ignored.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>, EnvParseError> {
    let mut parser = Parser::default();
    for c in text.chars() {
        parser.state = if is_line_break(c) && parser.state.ends_at_line_break() {
            parser.end_line()?
        } else {
            parser.step(c)
        };
        if c == '\n' {
            parser.line += 1;
        }
    }
    parser.finish()
}

struct Parser {
    pairs: Vec<(String, String)>,
    state: State,
    key: String,
    value: String,
    /// Length of `value` up to its last character that is not unquoted,
    /// unescaped trailing whitespace, which systemd trims.
    value_keep: usize,
    line: usize,
    key_line: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            pairs: Vec::new(),
            state: State::PreKey,
            key: String::new(),
            value: String::new(),
            value_keep: 0,
            line: 1,
            key_line: 1,
        }
    }
}

impl Parser {
    /// The state after `c`, for a character that does not end a line.
    fn step(&mut self, c: char) -> State {
        match self.state {
            State::PreKey => self.pre_key(c),
            State::Key => self.in_key(c),
            State::PreValue => self.pre_value(c),
            State::Value { escaped } => self.in_value(c, escaped),
            State::SingleQuote => self.single_quoted(c),
            State::DoubleQuote { escaped } => self.double_quoted(c, escaped),
            State::Comment { escaped } => State::Comment {
                escaped: !escaped && c == '\\',
            },
        }
    }

    fn pre_key(&mut self, c: char) -> State {
        if c == '#' || c == ';' {
            return State::Comment { escaped: false };
        }
        if c.is_ascii_whitespace() {
            return State::PreKey;
        }
        self.key_line = self.line;
        self.key.push(c);
        State::Key
    }

    fn in_key(&mut self, c: char) -> State {
        if c == '=' {
            self.value.clear();
            self.value_keep = 0;
            return State::PreValue;
        }
        self.key.push(c);
        State::Key
    }

    fn pre_value(&mut self, c: char) -> State {
        match c {
            '\'' => State::SingleQuote,
            '"' => State::DoubleQuote { escaped: false },
            '\\' => State::Value { escaped: true },
            _ if is_blank(c) => State::PreValue,
            _ => {
                self.push_kept(c);
                State::Value { escaped: false }
            }
        }
    }

    fn in_value(&mut self, c: char, escaped: bool) -> State {
        if escaped {
            if c != '\n' {
                self.push_kept(c);
            }
        } else if c == '\\' {
            return State::Value { escaped: true };
        } else if is_blank(c) {
            self.value.push(c);
        } else {
            self.push_kept(c);
        }
        State::Value { escaped: false }
    }

    fn single_quoted(&mut self, c: char) -> State {
        if c == '\'' {
            return State::PreValue;
        }
        self.push_kept(c);
        State::SingleQuote
    }

    fn double_quoted(&mut self, c: char, escaped: bool) -> State {
        if escaped {
            if DOUBLE_QUOTE_ESCAPED.contains(&c) {
                self.value.push(c);
            } else if c != '\n' {
                self.value.push('\\');
                self.value.push(c);
            }
            self.value_keep = self.value.len();
            return State::DoubleQuote { escaped: false };
        }
        match c {
            '"' => State::PreValue,
            '\\' => State::DoubleQuote { escaped: true },
            _ => {
                self.push_kept(c);
                State::DoubleQuote { escaped: false }
            }
        }
    }

    fn push_kept(&mut self, c: char) {
        self.value.push(c);
        self.value_keep = self.value.len();
    }

    /// Ends the current line, or the file: a key needs its `=`, a value is
    /// stored, a comment is over.
    fn end_line(&mut self) -> Result<State, EnvParseError> {
        match self.state {
            State::Key => Err(EnvParseError {
                line: self.line,
                problem: "a line without `=`",
            }),
            State::PreValue | State::Value { .. } => {
                self.commit()?;
                Ok(State::PreKey)
            }
            _ => Ok(State::PreKey),
        }
    }

    fn finish(mut self) -> Result<Vec<(String, String)>, EnvParseError> {
        match self.state {
            State::SingleQuote | State::DoubleQuote { .. } => {
                return Err(EnvParseError {
                    line: self.key_line,
                    problem: "an unterminated quote",
                });
            }
            State::PreKey | State::Comment { .. } => {}
            State::Key | State::PreValue | State::Value { .. } => {
                self.end_line()?;
            }
        }
        Ok(self.pairs)
    }

    fn commit(&mut self) -> Result<(), EnvParseError> {
        let name = self.key.trim_end().to_owned();
        if !valid_key(&name) {
            return Err(EnvParseError {
                line: self.key_line,
                problem: "invalid variable name",
            });
        }
        self.value.truncate(self.value_keep);
        self.pairs.retain(|(existing, _)| existing != &name);
        self.pairs.push((name, std::mem::take(&mut self.value)));
        self.key.clear();
        Ok(())
    }
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
    fn escapes_line_breaks_and_the_end_of_the_file() {
        let text = "\
LEADING=\\ x\n\
CONTINUED=a\\\nb\n\
QUOTED=\"a\\\nb\"\n\
CRLF=1\r\n\
TRIMMED=x \t\n\
# comment ending in an escaped line break \\\n\
STILL=comment\n\
ESCAPED_AT_END=y\\";
        let parsed = parse_env_file(text).unwrap();
        let pairs: Vec<(&str, &str)> = parsed
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("LEADING", " x"),
                ("CONTINUED", "ab"),
                ("QUOTED", "ab"),
                ("CRLF", "1"),
                ("TRIMMED", "x"),
                ("ESCAPED_AT_END", "y"),
            ]
        );
        assert_eq!(parse_env_file("# only \\").unwrap(), []);
        assert_eq!(parse_env_file("A=\"x\" \t'y'").unwrap()[0].1, "xy");
    }

    #[test]
    fn broken_files_are_errors_without_content() {
        for (text, line) in [
            ("A=1\nB=\"secret\n", 2),
            ("A=1\nnot an assignment\n", 2),
            ("1A=x\n", 1),
            ("A=1\nB='x", 2),
            ("A=1\nB", 2),
            ("A=1\r\nB\r\n", 2),
            ("A=1\nB C=x\n", 2),
            ("A=1\nB=\"x\\", 2),
        ] {
            let error = parse_env_file(text).unwrap_err();
            assert_eq!(error.line, line, "{text:?}");
            assert!(!error.to_string().contains("secret"));
        }
    }
}
