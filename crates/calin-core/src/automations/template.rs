//! An automation's prompt template (integrations spec, section 3):
//! `{{date}}` always, and `{{number}}`, `{{title}}`, `{{body}}`, `{{url}}`
//! and `{{author}}` for an issue or pull request trigger. Whitespace just
//! inside the braces is ignored. A placeholder that is not offered is
//! refused when the automation is saved, never when it runs.

/// A value a template can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Var {
    Date,
    Number,
    Title,
    Body,
    Url,
    Author,
}

const NAMES: [(&str, Var); 6] = [
    ("date", Var::Date),
    ("number", Var::Number),
    ("title", Var::Title),
    ("body", Var::Body),
    ("url", Var::Url),
    ("author", Var::Author),
];

/// What a schedule offers.
pub(crate) const SCHEDULE_VARS: &[Var] = &[Var::Date];
/// What an issue or pull request trigger offers.
pub(crate) const ITEM_VARS: &[Var] = &[
    Var::Date,
    Var::Number,
    Var::Title,
    Var::Body,
    Var::Url,
    Var::Author,
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Text(String),
    Var(Var),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template {
    segments: Vec<Segment>,
}

/// The values of one fire. What the trigger does not offer is empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Values {
    pub date: String,
    pub number: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
}

impl Template {
    /// Parses `source`, offering `allowed`. The error names the placeholder.
    pub(crate) fn parse(source: &str, allowed: &[Var]) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut rest = source;
        while let Some(open) = rest.find("{{") {
            if open > 0 {
                segments.push(Segment::Text(rest[..open].to_owned()));
            }
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else {
                return Err(format!(
                    "`{{{{` is never closed in the prompt: `{}`",
                    excerpt(&rest[open..])
                ));
            };
            let name = after[..close].trim();
            let var = NAMES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, var)| *var)
                .filter(|var| allowed.contains(var))
                .ok_or_else(|| unknown(name, allowed))?;
            segments.push(Segment::Var(var));
            rest = &after[close + 2..];
        }
        if !rest.is_empty() {
            segments.push(Segment::Text(rest.to_owned()));
        }
        Ok(Self { segments })
    }

    /// The prompt of one fire. Substituted text is never read again, so a
    /// title that itself holds `{{body}}` stays as it is.
    pub(crate) fn render(&self, values: &Values) -> String {
        let mut text = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(literal) => text.push_str(literal),
                Segment::Var(var) => text.push_str(values.get(*var)),
            }
        }
        text
    }
}

impl Values {
    fn get(&self, var: Var) -> &str {
        match var {
            Var::Date => &self.date,
            Var::Number => &self.number,
            Var::Title => &self.title,
            Var::Body => &self.body,
            Var::Url => &self.url,
            Var::Author => &self.author,
        }
    }
}

fn unknown(name: &str, allowed: &[Var]) -> String {
    let offered: Vec<String> = NAMES
        .iter()
        .filter(|(_, var)| allowed.contains(var))
        .map(|(known, _)| format!("{{{{{known}}}}}"))
        .collect();
    format!(
        "the prompt uses `{{{{{name}}}}}`, which this trigger does not offer (it offers {})",
        offered.join(", ")
    )
}

fn excerpt(text: &str) -> String {
    const CHARS: usize = 40;
    let mut cut: String = text.chars().take(CHARS).collect();
    if text.chars().count() > CHARS {
        cut.push_str("...");
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            date: "2026-11-28".into(),
            number: "12".into(),
            title: "Login {{body}} fails".into(),
            body: "Steps".into(),
            url: "https://x/12".into(),
            author: "octocat".into(),
        }
    }

    #[test]
    fn placeholders_are_filled_and_substituted_text_stays_literal() {
        let template = Template::parse(
            "Triage #{{number}} ({{ title }}) by {{author}} on {{date}}: {{url}}\n{{body}}",
            ITEM_VARS,
        )
        .unwrap();
        assert_eq!(
            template.render(&values()),
            "Triage #12 (Login {{body}} fails) by octocat on 2026-11-28: https://x/12\nSteps"
        );
        let plain = Template::parse("No placeholder, } and { alone.", SCHEDULE_VARS).unwrap();
        assert_eq!(plain.render(&values()), "No placeholder, } and { alone.");
        let date = Template::parse("{{date}}", SCHEDULE_VARS).unwrap();
        assert_eq!(date.render(&values()), "2026-11-28");
    }

    #[test]
    fn unknown_or_unoffered_placeholders_and_open_braces_are_refused() {
        for (source, allowed, problem) in [
            (
                "Hello {{name}}",
                ITEM_VARS,
                "`{{name}}`, which this trigger does not offer",
            ),
            ("Fix {{title}}", SCHEDULE_VARS, "it offers {{date}})"),
            ("{{}}", ITEM_VARS, "`{{}}`"),
            ("{{ Date }}", ITEM_VARS, "`{{Date}}`"),
            (
                "Start {{date",
                SCHEDULE_VARS,
                "`{{` is never closed in the prompt: `{{date`",
            ),
        ] {
            let err = Template::parse(source, allowed).unwrap_err();
            assert!(err.contains(problem), "{source}: {err}");
        }
        let all = Template::parse("{{x}}", ITEM_VARS).unwrap_err();
        assert!(
            all.contains("{{date}}, {{number}}, {{title}}, {{body}}, {{url}}, {{author}}"),
            "{all}"
        );
    }
}
