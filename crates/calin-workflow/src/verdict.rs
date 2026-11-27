//! Verdicts of `agent` nodes (spec section 3): the sentence appended to the prompt and the
//! parser for the answer live together so they cannot drift apart.

use serde::{Deserialize, Serialize};

/// The keyword of a verdict line, `VERDICT: <word>`. Matched case-insensitively.
pub const VERDICT_KEYWORD: &str = "VERDICT";

/// The port an agent with verdicts leaves through when no verdict is recognized.
pub const FAILED_PORT: &str = "failed";

/// The sentence appended (after a blank line) to the prompt of a node with verdicts.
pub fn verdict_instruction(verdicts: &[String]) -> String {
    let words: Vec<String> = verdicts.iter().map(|v| format!("`{v}`")).collect();
    format!(
        "End your answer with your verdict alone on its own line, written exactly as \
         `{VERDICT_KEYWORD}: <word>` where <word> is one of {}.",
        words.join(", ")
    )
}

/// What [`extract_verdict`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "word", rename_all = "snake_case")]
pub enum Verdict {
    /// One of the node's verdicts; the node leaves through the port of that name.
    Word(String),
    /// No verdict line, or the last one names no verdict of the node: the `failed` port.
    Unrecognized,
}

impl Verdict {
    /// The port to leave through.
    pub fn port(&self) -> &str {
        match self {
            Verdict::Word(w) => w,
            Verdict::Unrecognized => FAILED_PORT,
        }
    }

    /// The `verdict` output: the word, or empty.
    pub fn word(&self) -> &str {
        match self {
            Verdict::Word(w) => w,
            Verdict::Unrecognized => "",
        }
    }
}

/// Finds the verdict in an agent's final text.
///
/// A verdict line is a line that, with surrounding whitespace removed, is `VERDICT:` (keyword
/// in any case) followed by the rest of the line. The last verdict line wins, even when its
/// word is not one of `verdicts`: then the answer has no recognizable verdict. The word is
/// compared exactly, after trimming whitespace around it.
pub fn extract_verdict(text: &str, verdicts: &[String]) -> Verdict {
    let last = text.lines().rev().find_map(|line| {
        let (head, rest) = line.trim().split_once(':')?;
        head.eq_ignore_ascii_case(VERDICT_KEYWORD)
            .then(|| rest.trim())
    });
    match last {
        Some(word) if verdicts.iter().any(|v| v == word) => Verdict::Word(word.to_owned()),
        _ => Verdict::Unrecognized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn table() {
        let verdicts = v(&["approve", "changes", "needs-work"]);
        let cases: &[(&str, &str)] = &[
            ("VERDICT: approve", "approve"),
            ("Looks good.\n\nVERDICT: approve\n", "approve"),
            ("verdict: changes", "changes"),
            ("Verdict: approve", "approve"),
            ("   VERDICT:   approve   ", "approve"),
            ("\tVERDICT:approve", "approve"),
            ("VERDICT: needs-work\r\n", "needs-work"),
            // Last verdict line wins.
            ("VERDICT: approve\nmore text\nVERDICT: changes", "changes"),
            ("VERDICT: changes\nVERDICT: approve", "approve"),
            // The last line names an unknown word: no fallback to an earlier line.
            ("VERDICT: approve\nVERDICT: maybe", "failed"),
            // The word is exact.
            ("VERDICT: Approve", "failed"),
            ("VERDICT: APPROVE", "failed"),
            ("VERDICT: approve.", "failed"),
            ("VERDICT: approve now", "failed"),
            ("VERDICT:", "failed"),
            // Not alone on its line.
            ("I think VERDICT: approve", "failed"),
            ("**VERDICT: approve**", "failed"),
            ("VERDICT : approve", "failed"),
            ("", "failed"),
            ("no verdict at all", "failed"),
            // `failed` can never be a verdict, so it is not recognized as one.
            ("VERDICT: failed", "failed"),
        ];
        for (text, port) in cases {
            assert_eq!(extract_verdict(text, &verdicts).port(), *port, "{text:?}");
        }
    }

    #[test]
    fn instruction_round_trips_through_parser() {
        let verdicts = v(&["approve", "changes"]);
        let sentence = verdict_instruction(&verdicts);
        assert!(sentence.contains("`VERDICT: <word>`"), "{sentence}");
        assert!(sentence.contains("`approve`") && sentence.contains("`changes`"));
        for word in &verdicts {
            // An agent following the sentence literally writes `KEYWORD: word`.
            let answer = format!("Done.\n{VERDICT_KEYWORD}: {word}\n");
            assert_eq!(
                extract_verdict(&answer, &verdicts),
                Verdict::Word(word.clone())
            );
        }
    }
}
