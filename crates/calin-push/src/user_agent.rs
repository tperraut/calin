//! A device label from a user-agent string, when the browser sent none.
//! The same heuristics as the web app's `describeUserAgent`.

use crate::secret::clean_text;

const RAW_MAX_CHARS: usize = 60;

// Order matters: Edge and Opera also say Chrome, Chrome also says Safari.
const BROWSERS: &[(&[&str], &str)] = &[
    (&["Edg/", "Edge/", "EdgA/", "EdgiOS/"], "Edge"),
    (&["OPR/", "Opera/"], "Opera"),
    (&["SamsungBrowser/"], "Samsung Internet"),
    (&["Firefox/", "FxiOS/"], "Firefox"),
    (
        &["HeadlessChrome/", "Chrome/", "CriOS/", "Chromium/"],
        "Chrome",
    ),
];

const SYSTEMS: &[(&[&str], &str)] = &[
    (&["iPad"], "iPadOS"),
    (&["iPhone", "iPod"], "iOS"),
    (&["Android"], "Android"),
    (&["CrOS"], "ChromeOS"),
    (&["Windows"], "Windows"),
    (&["Mac OS X", "Macintosh"], "macOS"),
    (&["Linux"], "Linux"),
];

/// "Firefox on Linux", "Safari on iOS"; the raw string, cut short, when the
/// browser is not one of the common ones; "Unknown browser" when empty.
pub fn describe_user_agent(user_agent: &str) -> String {
    let raw = clean_text(user_agent, usize::MAX);
    if raw.is_empty() {
        return "Unknown browser".to_owned();
    }
    let browser = first_match(&raw, BROWSERS).or_else(|| {
        // Safari is the one that says `Version/` before `Safari/`.
        let version = find_token(&raw, "Version/")?;
        raw[version..].contains("Safari/").then_some("Safari")
    });
    match (browser, first_match(&raw, SYSTEMS)) {
        (Some(browser), Some(system)) => format!("{browser} on {system}"),
        (Some(browser), None) => browser.to_owned(),
        (None, _) if raw.chars().count() > RAW_MAX_CHARS => {
            let mut cut: String = raw.chars().take(RAW_MAX_CHARS - 1).collect();
            cut.push('\u{2026}');
            cut
        }
        (None, _) => raw,
    }
}

fn first_match(text: &str, table: &[(&[&str], &'static str)]) -> Option<&'static str> {
    table
        .iter()
        .find(|(tokens, _)| tokens.iter().any(|token| find_token(text, token).is_some()))
        .map(|(_, name)| *name)
}

/// Where `token` starts as a word: not preceded by a letter or digit.
fn find_token(text: &str, token: &str) -> Option<usize> {
    text.match_indices(token).map(|(at, _)| at).find(|&at| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_browsers_are_named() {
        let cases = [
            (
                "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
                "Firefox on Linux",
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1",
                "Safari on iOS",
            ),
            (
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
                "Chrome on Android",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
                "Edge on Windows",
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
                "Safari on macOS",
            ),
            (
                "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/120.0 Mobile/15E148 Safari/604.1",
                "Chrome on iPadOS",
            ),
            (
                "Mozilla/5.0 (Linux; Android 14; SAMSUNG SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/25.0 Chrome/121.0.0.0 Mobile Safari/537.36",
                "Samsung Internet on Android",
            ),
            ("Firefox/140", "Firefox"),
        ];
        for (ua, expected) in cases {
            assert_eq!(describe_user_agent(ua), expected, "{ua}");
        }
    }

    #[test]
    fn unknown_agents_are_shown_raw_and_short() {
        assert_eq!(describe_user_agent(""), "Unknown browser");
        assert_eq!(describe_user_agent(" \n "), "Unknown browser");
        assert_eq!(describe_user_agent("curl/8.0"), "curl/8.0");
        let long = "x".repeat(100);
        let shown = describe_user_agent(&long);
        assert_eq!(shown.chars().count(), RAW_MAX_CHARS);
        assert!(shown.ends_with('\u{2026}'));
        assert_eq!(describe_user_agent("bot\u{1b}[31m"), "bot[31m");
        // A token must start a word.
        assert_eq!(describe_user_agent("NotFirefox/1"), "NotFirefox/1");
    }
}
