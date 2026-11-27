use super::*;

/// Paris rules, spelled out so the tests do not depend on the machine's
/// time zone database: summer time from the last Sunday of March at 02:00
/// to the last Sunday of October at 03:00.
fn paris() -> TimeZone {
    TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
}

/// A local time that exists once, in Unix milliseconds.
fn at(zone: &TimeZone, text: &str) -> i64 {
    let local: DateTime = text.parse().unwrap();
    zone.to_ambiguous_timestamp(local)
        .unambiguous()
        .unwrap()
        .as_millisecond()
}

/// An instant as local time with its offset, which tells the two passes of
/// a fold apart.
fn show(zone: &TimeZone, ms: i64) -> String {
    let timestamp = Timestamp::from_millisecond(ms).unwrap();
    let offset = zone.to_offset(timestamp);
    format!("{} {offset}", zone.to_datetime(timestamp))
}

fn next(expr: &str, zone: &TimeZone, after: &str, count: usize) -> Vec<String> {
    let cron = Cron::parse(expr).unwrap_or_else(|err| panic!("{expr}: {err}"));
    cron.next_times(at(zone, after), zone, count)
        .into_iter()
        .map(|ms| show(zone, ms))
        .collect()
}

#[test]
fn invalid_expressions_say_what_is_wrong() {
    for (expr, problem) in [
        ("", "the expression is empty"),
        ("   ", "the expression is empty"),
        ("* * * *", "expected five fields"),
        ("* * * * * *", "found 6"),
        (
            "@yearly",
            "the supported aliases are @hourly, @daily, @weekly and @monthly",
        ),
        ("@", "unknown alias `@`"),
        ("@daily *", "found 2"),
        ("60 * * * *", "the minute field: 60 is out of range 0-59"),
        ("* 24 * * *", "the hour field: 24 is out of range 0-23"),
        (
            "* * 0 * *",
            "the day-of-month field: 0 is out of range 1-31",
        ),
        ("* * 32 * *", "the day-of-month field"),
        ("* * * 13 *", "the month field: 13 is out of range 1-12"),
        ("* * * * 8", "the day-of-week field: 8 is out of range 0-7"),
        ("5-1 * * * *", "the range `5-1` runs backwards"),
        ("*/0 * * * *", "the step of `*/0` must be between 1 and 60"),
        ("*/61 * * * *", "must be between 1 and 60"),
        ("*/x * * * *", "the step of `*/x` is not a number"),
        ("1,,2 * * * *", "has an empty item"),
        ("1, * * * *", "has an empty item"),
        ("a * * * *", "`a` is not a number"),
        ("-1 * * * *", "is not a number"),
        ("* * * foo *", "neither a number nor a name like `jan`"),
        ("* * * * mon-", "neither a number nor a name like `sun`"),
        ("* * * * L", "the day-of-week field"),
        ("* * ? * *", "the day-of-month field"),
        ("0 0 30 2 *", "it never fires"),
        ("0 0 31 4,6,9,11 *", "it never fires"),
    ] {
        let err = Cron::parse(expr).unwrap_err();
        assert!(err.contains(problem), "{expr:?}: {err}");
    }
    // A day that exists only some years is fine.
    assert!(Cron::parse("0 0 29 2 *").is_ok());
    // And a day no month has is fine when a weekday may match instead.
    assert!(Cron::parse("0 0 30 2 mon").is_ok());
}

#[test]
fn fields_take_values_ranges_lists_steps_and_names() {
    let utc = TimeZone::UTC;
    let after = "2026-11-28T10:07:30";
    for (expr, expected) in [
        (
            "* * * * *",
            vec!["2026-11-28T10:08:00", "2026-11-28T10:09:00"],
        ),
        (
            "*/15 * * * *",
            vec!["2026-11-28T10:15:00", "2026-11-28T10:30:00"],
        ),
        (
            "5/20 * * * *",
            vec!["2026-11-28T10:25:00", "2026-11-28T10:45:00"],
        ),
        (
            "10-20/5 * * * *",
            vec!["2026-11-28T10:10:00", "2026-11-28T10:15:00"],
        ),
        (
            "0,30 9-11 * * *",
            vec!["2026-11-28T10:30:00", "2026-11-28T11:00:00"],
        ),
        (
            "0 8 * * *",
            vec!["2026-11-29T08:00:00", "2026-11-30T08:00:00"],
        ),
        // Saturday the 28th: the next weekdays are Monday and Tuesday.
        (
            "0 8 * * 1-5",
            vec!["2026-11-30T08:00:00", "2026-12-01T08:00:00"],
        ),
        (
            "0 8 * * MON-fri",
            vec!["2026-11-30T08:00:00", "2026-12-01T08:00:00"],
        ),
        // 7 is Sunday, like 0.
        (
            "0 8 * * 7",
            vec!["2026-11-29T08:00:00", "2026-12-06T08:00:00"],
        ),
        (
            "0 8 * * 5-7",
            vec!["2026-11-29T08:00:00", "2026-12-04T08:00:00"],
        ),
        (
            "0 0 1 * *",
            vec!["2026-12-01T00:00:00", "2027-01-01T00:00:00"],
        ),
        (
            "0 0 1 jan,Jul *",
            vec!["2027-01-01T00:00:00", "2027-07-01T00:00:00"],
        ),
        (
            "0 12 * 2 *",
            vec!["2027-02-01T12:00:00", "2027-02-02T12:00:00"],
        ),
        (
            "0 0 29 2 *",
            vec!["2028-02-29T00:00:00", "2032-02-29T00:00:00"],
        ),
        (
            "59 23 31 * *",
            vec!["2026-12-31T23:59:00", "2027-01-31T23:59:00"],
        ),
        // Both day fields restricted: either matches (the 1st, or a Friday).
        (
            "0 9 1 * fri",
            vec!["2026-12-01T09:00:00", "2026-12-04T09:00:00"],
        ),
        // One of them starts with `*`: both must match (odd days that are Mondays).
        (
            "0 9 */2 * mon",
            vec!["2026-12-07T09:00:00", "2026-12-21T09:00:00"],
        ),
        // The 13th that is a Friday, written the other way round, is the AND form.
        (
            "0 0 13 * *",
            vec!["2026-12-13T00:00:00", "2027-01-13T00:00:00"],
        ),
    ] {
        let expected: Vec<String> = expected.iter().map(|time| format!("{time} +00")).collect();
        assert_eq!(next(expr, &utc, after, 2), expected, "{expr}");
    }
}

#[test]
fn the_next_time_is_strictly_after_and_in_local_time() {
    let tokyo = TimeZone::fixed(jiff::tz::offset(9));
    // On the minute: that minute is not "after".
    assert_eq!(
        next("0 8 * * *", &tokyo, "2026-11-28T08:00:00", 1),
        ["2026-11-29T08:00:00 +09"]
    );
    assert_eq!(
        next("0 8 * * *", &tokyo, "2026-11-28T07:59:59.999", 1),
        ["2026-11-28T08:00:00 +09"]
    );
    // 08:00 in Tokyo is 23:00 the day before in UTC.
    let cron = Cron::parse("0 8 * * *").unwrap();
    let fired = cron
        .next_after(at(&TimeZone::UTC, "2026-11-28T00:00:00"), &tokyo)
        .unwrap();
    assert_eq!(show(&TimeZone::UTC, fired), "2026-11-28T23:00:00 +00");
}

#[test]
fn a_time_in_the_spring_gap_is_skipped_that_day() {
    let paris = paris();
    // 02:30 does not exist on 2026-03-29 in Paris.
    assert_eq!(
        next("30 2 * * *", &paris, "2026-03-28T12:00:00", 3),
        [
            "2026-03-30T02:30:00 +02",
            "2026-03-31T02:30:00 +02",
            "2026-04-01T02:30:00 +02",
        ]
    );
    // Every 30 minutes through the gap: 01:30, then 03:00.
    assert_eq!(
        next("*/30 * * * *", &paris, "2026-03-29T01:00:00", 3),
        [
            "2026-03-29T01:30:00 +01",
            "2026-03-29T03:00:00 +02",
            "2026-03-29T03:30:00 +02",
        ]
    );
}

#[test]
fn a_time_in_the_autumn_fold_fires_once() {
    let paris = paris();
    // 02:30 happens twice on 2026-10-25 in Paris: only the first counts.
    assert_eq!(
        next("30 2 * * *", &paris, "2026-10-24T12:00:00", 2),
        ["2026-10-25T02:30:00 +02", "2026-10-26T02:30:00 +01"]
    );
    // Every 30 minutes through the fold: the repeated hour is not repeated.
    assert_eq!(
        next("*/30 * * * *", &paris, "2026-10-25T01:45:00", 4),
        [
            "2026-10-25T02:00:00 +02",
            "2026-10-25T02:30:00 +02",
            "2026-10-25T03:00:00 +01",
            "2026-10-25T03:30:00 +01",
        ]
    );
    // From inside the second pass, the next fire is after it.
    let cron = Cron::parse("*/30 * * * *").unwrap();
    let first_pass_end = at(&paris, "2026-10-25T01:59:00") + 60 * 60 * 1000;
    let second_pass = first_pass_end + 31 * 60 * 1000;
    let fired = cron.next_after(second_pass, &paris).unwrap();
    assert_eq!(show(&paris, fired), "2026-10-25T03:00:00 +01");
}

#[test]
fn expressions_are_put_in_words_for_common_shapes() {
    for (expr, words) in [
        ("* * * * *", "Every minute"),
        ("*/15 * * * *", "Every 15 minutes"),
        ("0 * * * *", "Every hour"),
        ("20 * * * *", "Every hour at minute 20"),
        ("0 */2 * * *", "Every 2 hours"),
        ("0 8 * * *", "Every day at 08:00"),
        ("0   8  *  *  *", "Every day at 08:00"),
        ("30 9,17 * * *", "Every day at 09:30 and 17:30"),
        ("0 8 * * 1-5", "Every weekday at 08:00"),
        ("0 8 * * mon-fri", "Every weekday at 08:00"),
        ("30 9 * * 1", "At 09:30 on Mondays"),
        ("30 9 * * MON", "At 09:30 on Mondays"),
        (
            "0 18 * * 1,3,5",
            "At 18:00 on Mondays, Wednesdays and Fridays",
        ),
        ("0 10 * * 0,6", "At 10:00 on Sundays and Saturdays"),
        ("0 10 * * 7", "At 10:00 on Sundays"),
        ("0 0 1 * *", "On the 1st of every month at 00:00"),
        ("15 6 2 * *", "On the 2nd of every month at 06:15"),
        ("0 6 3 * *", "On the 3rd of every month at 06:00"),
        ("0 6 11 * *", "On the 11th of every month at 06:00"),
        ("0 6 22 * *", "On the 22nd of every month at 06:00"),
        ("0 6 31 * *", "On the 31st of every month at 06:00"),
        ("0 0 1 1 *", "Every year on January 1 at 00:00"),
        // Anything else is shown as it is.
        ("0 8 * 1-6 *", "Cron: 0 8 * 1-6 *"),
        ("*/5 9-17 * * 1-5", "Cron: */5 9-17 * * 1-5"),
        ("0 8 1 * 1", "Cron: 0 8 1 * 1"),
        ("0 0,4,8,12,16 * * *", "Cron: 0 0,4,8,12,16 * * *"),
        ("5,10 8 * * *", "Cron: 5,10 8 * * *"),
        ("0 8 */2 * *", "Cron: 0 8 */2 * *"),
        ("0   8 */2 * *", "Cron: 0 8 */2 * *"),
        ("*/7 * * * 1", "Cron: */7 * * * 1"),
    ] {
        assert_eq!(Cron::parse(expr).unwrap().describe(), words, "{expr}");
    }
}

#[test]
fn an_expression_that_only_fires_years_apart_is_found() {
    // Feb 29 that is a Monday, as an AND (the day field starts with `*`).
    let cron = Cron::parse("0 0 */29 2 mon").unwrap();
    let fired = cron
        .next_after(at(&TimeZone::UTC, "2026-01-01T00:00:00"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(show(&TimeZone::UTC, fired), "2027-02-01T00:00:00 +00");
    // Feb 29 alone: the next leap year.
    let leap = Cron::parse("0 0 29 2 *").unwrap();
    let fired = leap
        .next_after(at(&TimeZone::UTC, "2028-03-01T00:00:00"), &TimeZone::UTC)
        .unwrap();
    assert_eq!(show(&TimeZone::UTC, fired), "2032-02-29T00:00:00 +00");
}

#[test]
fn aliases_behave_like_their_expressions() {
    let zone = paris();
    for (alias, expr, words) in [
        ("@hourly", "0 * * * *", "Every hour"),
        ("@daily", "0 0 * * *", "Every day at 00:00"),
        ("@weekly", "0 0 * * 0", "At 00:00 on Sundays"),
        (
            "@monthly",
            "0 0 1 * *",
            "On the 1st of every month at 00:00",
        ),
    ] {
        let a = Cron::parse(alias).unwrap();
        let e = Cron::parse(expr).unwrap();
        assert_eq!(a.describe(), e.describe(), "{alias}");
        assert_eq!(
            next(alias, &zone, "2026-03-01T12:30:00", 40),
            next(expr, &zone, "2026-03-01T12:30:00", 40),
            "{alias}"
        );
        assert_eq!(a.describe(), words, "{alias}");
    }
    assert!(Cron::parse("  @Daily ").is_ok());
}
