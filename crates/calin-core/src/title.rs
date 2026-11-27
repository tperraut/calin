/// Longest title derived from a prompt, in characters.
const TITLE_CHARS: usize = 80;

/// The title of a session that was not given one: the first line of its prompt.
pub(crate) fn default_title(prompt: &str) -> String {
    prompt
        .trim()
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end()
        .chars()
        .take(TITLE_CHARS)
        .collect()
}

/// Longest excerpt of a message, in characters: what fits on a graph edge.
const EXCERPT_CHARS: usize = 80;

/// The first line of `text` that says something, cut to 80 characters with
/// an ellipsis when it is longer. Empty when the text is blank.
pub(crate) fn excerpt(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= EXCERPT_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(EXCERPT_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_is_the_first_line_of_the_prompt() {
        assert_eq!(
            default_title("\n  Fix the bug  \nmore detail"),
            "Fix the bug"
        );
        assert_eq!(default_title("one line"), "one line");
    }

    #[test]
    fn an_excerpt_is_one_line_of_at_most_80_characters() {
        assert_eq!(excerpt("\n\n  first line  \nsecond"), "first line");
        assert_eq!(excerpt(" \n\t\n"), "");
        let exact = "é".repeat(80);
        assert_eq!(excerpt(&exact), exact);
        let long = excerpt(&"é".repeat(81));
        assert_eq!(long.chars().count(), 80);
        assert_eq!(long, format!("{}…", "é".repeat(79)));
    }

    #[test]
    fn long_titles_are_cut_at_80_characters_not_bytes() {
        let prompt = "é".repeat(200);
        let title = default_title(&prompt);
        assert_eq!(title.chars().count(), 80);
        assert_eq!(title, "é".repeat(80));
    }
}
