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
    fn long_titles_are_cut_at_80_characters_not_bytes() {
        let prompt = "é".repeat(200);
        let title = default_title(&prompt);
        assert_eq!(title.chars().count(), 80);
        assert_eq!(title, "é".repeat(80));
    }
}
