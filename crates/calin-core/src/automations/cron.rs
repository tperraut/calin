//! Five-field cron expressions in the server's local time (integrations
//! spec, section 3): parsing, the next times one fires, and what it means
//! in words.
//!
//! The fields are minute (0-59), hour (0-23), day of month (1-31), month
//! (1-12 or `jan`-`dec`) and day of week (0-7, Sunday being 0 and 7, or
//! `sun`-`sat`). Each is `*` or a list of values and ranges (`1-5`), each
//! with an optional step (`*/15`, `8-18/2`, `5/10` from 5 to the end).
//! As in Vixie cron, when both day fields are restricted (neither starts
//! with `*`) a day matches either; otherwise it must match both.
//!
//! Daylight saving time: a local time that does not exist on a day (in the
//! gap of a spring change) is skipped that day; a local time that exists
//! twice (in the fold of an autumn change) fires once, at its first
//! occurrence. The aliases `@hourly`, `@daily`, `@weekly` and `@monthly` stand
//! for their five-field expressions; other macros, seconds, and `L`, `W`, `#`
//! and `?` are not supported.

use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{Timestamp, ToSpan};

/// How far ahead a next time is looked for: eight years cover every
/// combination of a day and a month, Feb 29 on a given weekday included.
const SEARCH_DAYS: u32 = 8 * 366;

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAY_PLURALS: [&str; 7] = [
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];

/// How a field was written, for putting it in words.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shape {
    /// `*`.
    Any,
    /// `*/n`.
    Every(u32),
    /// Anything else, as the values it allows, in order.
    Values(Vec<u32>),
}

/// One field: the values it allows as bits, and its shape.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    bits: u64,
    shape: Shape,
    /// Written with a leading `*`: unrestricted for the day-field rule.
    star: bool,
}

impl Field {
    fn has(&self, value: u32) -> bool {
        value < 64 && self.bits & (1 << value) != 0
    }

    fn values(&self, min: u32, max: u32) -> impl Iterator<Item = u32> + '_ {
        (min..=max).filter(|value| self.has(*value))
    }
}

/// A valid expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cron {
    /// The fields as written, separated by one space.
    source: String,
    minute: Field,
    hour: Field,
    day: Field,
    month: Field,
    weekday: Field,
}

struct Spec {
    name: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
    /// The first name's value.
    names_from: u32,
}

const SPECS: [Spec; 5] = [
    Spec {
        name: "minute",
        min: 0,
        max: 59,
        names: &[],
        names_from: 0,
    },
    Spec {
        name: "hour",
        min: 0,
        max: 23,
        names: &[],
        names_from: 0,
    },
    Spec {
        name: "day-of-month",
        min: 1,
        max: 31,
        names: &[],
        names_from: 0,
    },
    Spec {
        name: "month",
        min: 1,
        max: 12,
        names: &MONTHS,
        names_from: 1,
    },
    // 7 is Sunday too; it is folded into 0 once parsed.
    Spec {
        name: "day-of-week",
        min: 0,
        max: 7,
        names: &WEEKDAYS,
        names_from: 0,
    },
];

/// The `@` aliases and the expressions they stand for.
const ALIASES: [(&str, &str); 4] = [
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@weekly", "0 0 * * 0"),
    ("@monthly", "0 0 1 * *"),
];

impl Cron {
    /// Parses an expression. The error says what is wrong, in words.
    pub(crate) fn parse(expr: &str) -> Result<Self, String> {
        let fields = expand(expr.split_whitespace().collect())?;
        if fields.is_empty() {
            return Err("the expression is empty".to_owned());
        }
        if fields.len() != 5 {
            return Err(format!(
                "expected five fields (minute, hour, day of month, month, day of week), found {}",
                fields.len()
            ));
        }
        let parsed = fields
            .iter()
            .zip(&SPECS)
            .map(|(text, spec)| {
                parse_field(text, spec).map_err(|why| format!("the {} field: {why}", spec.name))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let [minute, hour, day, month, weekday]: [Field; 5] = parsed
            .try_into()
            .map_err(|_| "expected five fields".to_owned())?;
        let cron = Self {
            source: fields.join(" "),
            minute,
            hour,
            day,
            month,
            weekday: sunday_once(weekday),
        };
        if !cron.can_fire() {
            return Err("it never fires: no month has that day".to_owned());
        }
        Ok(cron)
    }

    /// True when some date matches the day and month fields.
    fn can_fire(&self) -> bool {
        // Any weekday comes round in every month, so only the day of month
        // can rule a month out: a day it does not have (Feb 30, Apr 31).
        let either = !self.day.star && !self.weekday.star;
        if either {
            return true;
        }
        const LONGEST: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        (1..=12).any(|month| {
            self.month.has(month)
                && self
                    .day
                    .values(1, LONGEST[month as usize - 1])
                    .next()
                    .is_some()
        })
    }

    fn day_matches(&self, date: Date) -> bool {
        let day = self.day.has(date.day() as u32);
        let weekday = self
            .weekday
            .has(date.weekday().to_sunday_zero_offset() as u32);
        if self.day.star || self.weekday.star {
            day && weekday
        } else {
            day || weekday
        }
    }

    /// The first time after `after_ms` (Unix milliseconds) that the
    /// expression fires in `zone`, in Unix milliseconds.
    pub(crate) fn next_after(&self, after_ms: i64, zone: &TimeZone) -> Option<i64> {
        let after = Timestamp::from_millisecond(after_ms).ok()?;
        let mut date = zone.to_datetime(after).date();
        for _ in 0..SEARCH_DAYS {
            if self.month.has(date.month() as u32) && self.day_matches(date) {
                for hour in self.hour.values(0, 23) {
                    for minute in self.minute.values(0, 59) {
                        let local = date.at(hour as i8, minute as i8, 0, 0);
                        let Some(at) = instant(zone, local) else {
                            continue;
                        };
                        if at > after_ms {
                            return Some(at);
                        }
                    }
                }
            }
            date = date.checked_add(1.day()).ok()?;
        }
        None
    }

    /// The next `count` times it fires after `after_ms`.
    pub(crate) fn next_times(&self, after_ms: i64, zone: &TimeZone, count: usize) -> Vec<i64> {
        let mut times = Vec::with_capacity(count);
        let mut from = after_ms;
        while times.len() < count {
            let Some(next) = self.next_after(from, zone) else {
                break;
            };
            times.push(next);
            from = next;
        }
        times
    }

    /// The expression in words, for the common shapes; `Cron: <expr>` for
    /// any other.
    pub(crate) fn describe(&self) -> String {
        self.words()
            .unwrap_or_else(|| format!("Cron: {}", self.source))
    }

    fn words(&self) -> Option<String> {
        let every_day = [&self.day, &self.month, &self.weekday]
            .iter()
            .all(|field| field.shape == Shape::Any);
        if every_day && let Some(words) = self.often() {
            return Some(words);
        }
        // From here on: one minute, at one or a few hours.
        let times = self.times()?;
        if every_day {
            return Some(format!("Every day at {times}"));
        }
        self.on_days(&times)
    }

    /// Every day, more than a few times: every minute, every n minutes,
    /// every hour, every n hours.
    fn often(&self) -> Option<String> {
        use Shape::{Any, Every, Values};
        match (&self.minute.shape, &self.hour.shape) {
            (Any, Any) => Some("Every minute".to_owned()),
            (Every(n), Any) => Some(format!("Every {n} minutes")),
            (Values(minutes), Any) if minutes.len() == 1 => Some(match minutes[0] {
                0 => "Every hour".to_owned(),
                minute => format!("Every hour at minute {minute}"),
            }),
            (Values(minutes), Every(n)) if minutes == &[0] => Some(format!("Every {n} hours")),
            _ => None,
        }
    }

    /// One minute at one to four hours, as `08:00 and 18:00`.
    fn times(&self) -> Option<String> {
        let (Shape::Values(minutes), Shape::Values(hours)) = (&self.minute.shape, &self.hour.shape)
        else {
            return None;
        };
        if minutes.len() != 1 || hours.len() > 4 {
            return None;
        }
        let times: Vec<String> = hours
            .iter()
            .map(|hour| format!("{hour:02}:{:02}", minutes[0]))
            .collect();
        Some(list(&times))
    }

    /// The common shapes of days that are not every day.
    fn on_days(&self, times: &str) -> Option<String> {
        use Shape::{Any, Values};
        match (&self.day.shape, &self.month.shape, &self.weekday.shape) {
            (Any, Any, Values(days)) if days == &[1, 2, 3, 4, 5] => {
                Some(format!("Every weekday at {times}"))
            }
            (Any, Any, Values(days)) => {
                let names: Vec<String> = days
                    .iter()
                    .map(|day| WEEKDAY_PLURALS[*day as usize].to_owned())
                    .collect();
                Some(format!("At {times} on {}", list(&names)))
            }
            (Values(days), Any, Any) if days.len() == 1 => Some(format!(
                "On the {} of every month at {times}",
                ordinal(days[0])
            )),
            (Values(days), Values(months), Any) if days.len() == 1 && months.len() == 1 => {
                Some(format!(
                    "Every year on {} {} at {times}",
                    MONTH_NAMES[months[0] as usize - 1],
                    days[0]
                ))
            }
            _ => None,
        }
    }
}

/// The instant of a local time, or `None` when the time does not exist
/// that day. One that exists twice is its first occurrence.
fn instant(zone: &TimeZone, local: DateTime) -> Option<i64> {
    let ambiguous = zone.to_ambiguous_timestamp(local);
    let timestamp = match ambiguous.offset() {
        AmbiguousOffset::Gap { .. } => return None,
        AmbiguousOffset::Unambiguous { .. } | AmbiguousOffset::Fold { .. } => {
            ambiguous.earlier().ok()?
        }
    };
    Some(timestamp.as_millisecond())
}

/// An `@` alias as the five fields it stands for; other fields as they are.
fn expand(fields: Vec<&str>) -> Result<Vec<&str>, String> {
    let [first] = fields[..] else {
        return Ok(fields);
    };
    if !first.starts_with('@') {
        return Ok(fields);
    }
    let Some((_, expansion)) = ALIASES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(first))
    else {
        return Err(format!(
            "unknown alias `{first}`; the supported aliases are @hourly, @daily, @weekly and @monthly"
        ));
    };
    // Expanding here makes an alias identical to its expression everywhere.
    Ok(expansion.split(' ').collect())
}

/// Sunday is 0 and 7: keeps it as 0 only.
fn sunday_once(mut weekday: Field) -> Field {
    if weekday.has(7) {
        weekday.bits = (weekday.bits & !(1 << 7)) | 1;
    }
    if let Shape::Values(values) = &mut weekday.shape {
        for value in values.iter_mut().filter(|value| **value == 7) {
            *value = 0;
        }
        values.sort_unstable();
        values.dedup();
    }
    weekday
}

fn parse_field(text: &str, spec: &Spec) -> Result<Field, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        bits |= item_bits(item, text, spec)?;
    }
    Ok(Field {
        bits,
        shape: shape(text, bits, spec),
        star: text.starts_with('*'),
    })
}

/// The values one item of the field `text` allows, as bits.
fn item_bits(item: &str, text: &str, spec: &Spec) -> Result<u64, String> {
    if item.is_empty() {
        return Err(format!("`{text}` has an empty item"));
    }
    let (base, step) = split_step(item, spec)?;
    let (from, to) = range(base, step.is_some(), spec)?;
    let every = step.unwrap_or(1) as usize;
    Ok((from..=to)
        .step_by(every)
        .fold(0, |bits, at| bits | 1 << at))
}

/// An item without its step, and the step.
fn split_step<'a>(item: &'a str, spec: &Spec) -> Result<(&'a str, Option<u32>), String> {
    let Some((base, step)) = item.split_once('/') else {
        return Ok((item, None));
    };
    let step: u32 = step
        .parse()
        .map_err(|_| format!("the step of `{item}` is not a number"))?;
    let most = spec.max - spec.min + 1;
    if step == 0 || step > most {
        return Err(format!("the step of `{item}` must be between 1 and {most}"));
    }
    Ok((base, Some(step)))
}

/// The first and the last value an item allows, before its step.
fn range(base: &str, stepped: bool, spec: &Spec) -> Result<(u32, u32), String> {
    if base == "*" {
        return Ok((spec.min, spec.max));
    }
    if let Some((from, to)) = base.split_once('-') {
        let (from, to) = (value(from, spec)?, value(to, spec)?);
        if from > to {
            return Err(format!("the range `{base}` runs backwards"));
        }
        return Ok((from, to));
    }
    let from = value(base, spec)?;
    // `5/10` steps from 5 to the end, as in Vixie cron.
    Ok((from, if stepped { spec.max } else { from }))
}

/// How the field was written, for putting it in words.
fn shape(text: &str, bits: u64, spec: &Spec) -> Shape {
    if text == "*" {
        return Shape::Any;
    }
    if let Some(step) = text.strip_prefix("*/").and_then(|step| step.parse().ok()) {
        return Shape::Every(step);
    }
    let values = (spec.min..=spec.max).filter(|v| bits & (1 << v) != 0);
    Shape::Values(values.collect())
}

fn value(text: &str, spec: &Spec) -> Result<u32, String> {
    let lower = text.to_ascii_lowercase();
    if let Some(index) = spec.names.iter().position(|name| *name == lower) {
        return Ok(spec.names_from + index as u32);
    }
    let value: u32 = text.parse().map_err(|_| {
        if spec.names.is_empty() {
            format!("`{text}` is not a number")
        } else {
            format!(
                "`{text}` is neither a number nor a name like `{}`",
                spec.names[0]
            )
        }
    })?;
    if !(spec.min..=spec.max).contains(&value) {
        return Err(format!("{value} is out of range {}-{}", spec.min, spec.max));
    }
    Ok(value)
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn ordinal(day: u32) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

#[cfg(test)]
mod tests;
