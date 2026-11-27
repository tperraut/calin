//! `GET /usage`, read from the `usage` projection (integrations spec,
//! section 1): totals and a breakdown over `[from, to)`.

use std::collections::{BTreeMap, HashMap, HashSet};

use calin_proto::{Usage, UsageBy, UsageRow, UsageTotals};
use jiff::ToSpan;
use rusqlite::{Connection, Row};

use super::index::{query_all, query_optional};
use crate::clock::{TimeZone, local_date};
use crate::error::{Error, Result};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The range when the request names none.
const DEFAULT_RANGE_MS: i64 = 30 * DAY_MS;
/// The longest range there is; a longer one is cut at its start.
const MAX_RANGE_MS: i64 = 366 * DAY_MS;
/// `by=session` lists the most expensive root sessions, no more.
pub(crate) const MAX_SESSION_ROWS: usize = 200;

const NO_REPO: &str = "No repo";
const UNKNOWN_MODEL: &str = "Unknown model";

/// What to add up.
#[derive(Clone, Debug)]
pub(crate) struct UsageQuery {
    /// Unix milliseconds, `from` included and `to` not.
    pub from: i64,
    pub to: i64,
    pub by: UsageBy,
    /// Where days begin and end.
    pub zone: TimeZone,
}

/// The range a request asks for, after the defaults (the last 30 days up to
/// `now`) and the clamping (at most 366 days, cut at the start). `Invalid`
/// when it ends before it starts or starts before 1970.
pub(crate) fn resolve_range(from: Option<i64>, to: Option<i64>, now: i64) -> Result<(i64, i64)> {
    // The end is exclusive: by default it is just after now, so what was
    // reported this very millisecond is in "the last 30 days".
    let to = to.unwrap_or(now.saturating_add(1));
    let from = from.unwrap_or(to.saturating_sub(DEFAULT_RANGE_MS));
    if from < 0 || to < 0 {
        return Err(Error::Invalid(
            "from and to are Unix milliseconds and cannot be negative".to_owned(),
        ));
    }
    if from >= to {
        return Err(Error::Invalid("from must be before to".to_owned()));
    }
    Ok((from.max(to - MAX_RANGE_MS), to))
}

#[derive(Default)]
struct Sum {
    cost_usd: f64,
    tokens_in: u64,
    tokens_out: u64,
    roots: HashSet<String>,
}

impl Sum {
    fn add(&mut self, row: &Spent) {
        self.cost_usd += row.cost_usd;
        self.tokens_in += row.tokens_in;
        self.tokens_out += row.tokens_out;
        self.roots.insert(row.root.clone());
    }

    fn totals(&self) -> UsageTotals {
        UsageTotals {
            cost_usd: self.cost_usd,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            sessions: self.roots.len() as u32,
        }
    }
}

/// One `usage` row of the range.
struct Spent {
    ts: i64,
    session_id: String,
    root: String,
    runner: String,
    model: Option<String>,
    cost_usd: f64,
    tokens_in: u64,
    tokens_out: u64,
}

impl Spent {
    fn added_something(&self) -> bool {
        self.cost_usd > 0.0 || self.tokens_in > 0 || self.tokens_out > 0
    }
}

pub(crate) fn usage(db: &Connection, query: &UsageQuery) -> Result<Usage> {
    let spent = spent(db, query)?;
    let mut total = Sum::default();
    for row in &spent {
        total.add(row);
    }
    Ok(Usage {
        from: query.from,
        to: query.to,
        by: query.by,
        totals: total.totals(),
        rows: breakdown(db, query, &spent)?,
    })
}

/// The `usage` rows of the range that added something.
fn spent(db: &Connection, query: &UsageQuery) -> Result<Vec<Spent>> {
    let all = query_all(
        db,
        "SELECT u.ts, u.session_id, s.root_session_id, u.runner, u.model, u.cost_usd, \
         u.tokens_in, u.tokens_out FROM usage u JOIN sessions s ON s.id = u.session_id \
         WHERE u.ts >= ?1 AND u.ts < ?2 ORDER BY u.ts, u.session_id, u.seq",
        [query.from, query.to],
        spent_from_row,
    )?;
    // A report that added nothing is no use, and no session "used" anything by it.
    Ok(all.into_iter().filter(Spent::added_something).collect())
}

fn spent_from_row(row: &Row<'_>) -> rusqlite::Result<Spent> {
    Ok(Spent {
        ts: row.get(0)?,
        session_id: row.get(1)?,
        root: row.get(2)?,
        runner: row.get(3)?,
        model: row.get(4)?,
        cost_usd: row.get(5)?,
        tokens_in: row.get::<_, i64>(6)?.max(0) as u64,
        tokens_out: row.get::<_, i64>(7)?.max(0) as u64,
    })
}

/// The rows of the breakdown `query.by` asks for.
fn breakdown(db: &Connection, query: &UsageQuery, spent: &[Spent]) -> Result<Vec<UsageRow>> {
    Ok(match query.by {
        UsageBy::Day => by_day(spent, query),
        UsageBy::Session => by_session(db, spent)?,
        UsageBy::Repo => by_repo(db, spent)?,
        UsageBy::Runner => grouped(spent, |row| row.runner.clone()),
        UsageBy::Model => by_model(spent)?,
    })
}

/// The most expensive root sessions, labelled with their titles.
fn by_session(db: &Connection, spent: &[Spent]) -> Result<Vec<UsageRow>> {
    let mut rows = grouped(spent, |row| row.root.clone());
    rows.truncate(MAX_SESSION_ROWS);
    label_rows(&mut rows, |key| title(db, key))?;
    Ok(rows)
}

/// By the primary repo of the session that spent, labelled with its name.
fn by_repo(db: &Connection, spent: &[Spent]) -> Result<Vec<UsageRow>> {
    let primaries = primary_repos(db)?;
    let none = (String::new(), NO_REPO.to_owned());
    let mut rows = grouped(spent, |row| {
        primaries.get(&row.session_id).unwrap_or(&none).0.clone()
    });
    let names: HashMap<String, String> = primaries.values().cloned().collect();
    label_rows(&mut rows, |key| Ok(Some(repo_label(&names, key))))?;
    Ok(rows)
}

fn repo_label(names: &HashMap<String, String>, key: &str) -> String {
    match names.get(key) {
        Some(name) => name.clone(),
        None if key.is_empty() => NO_REPO.to_owned(),
        None => key.to_owned(),
    }
}

fn by_model(spent: &[Spent]) -> Result<Vec<UsageRow>> {
    let mut rows = grouped(spent, |row| row.model.clone().unwrap_or_default());
    label_rows(&mut rows, |key| {
        Ok(Some(if key.is_empty() {
            UNKNOWN_MODEL.to_owned()
        } else {
            key.to_owned()
        }))
    })?;
    Ok(rows)
}

/// Every local day of the range in order, the empty ones included.
fn by_day(spent: &[Spent], query: &UsageQuery) -> Vec<UsageRow> {
    let mut days: BTreeMap<jiff::civil::Date, Sum> = BTreeMap::new();
    let (first, last) = (
        local_date(&query.zone, query.from),
        local_date(&query.zone, query.to - 1),
    );
    let mut day = first;
    while day <= last {
        days.insert(day, Sum::default());
        match day.checked_add(1.day()) {
            Ok(next) => day = next,
            Err(_) => break,
        }
    }
    for row in spent {
        days.entry(local_date(&query.zone, row.ts))
            .or_default()
            .add(row);
    }
    days.into_iter()
        .map(|(day, sum)| UsageRow {
            key: day.to_string(),
            label: day.to_string(),
            totals: sum.totals(),
        })
        .collect()
}

/// One row per key, the most expensive first, labelled with the key.
fn grouped(spent: &[Spent], key: impl Fn(&Spent) -> String) -> Vec<UsageRow> {
    let mut sums: HashMap<String, Sum> = HashMap::new();
    for row in spent {
        sums.entry(key(row)).or_default().add(row);
    }
    let mut rows: Vec<UsageRow> = sums
        .into_iter()
        .map(|(key, sum)| UsageRow {
            label: key.clone(),
            key,
            totals: sum.totals(),
        })
        .collect();
    rows.sort_by(|a, b| {
        let tokens = |row: &UsageRow| row.totals.tokens_in + row.totals.tokens_out;
        b.totals
            .cost_usd
            .total_cmp(&a.totals.cost_usd)
            .then(tokens(b).cmp(&tokens(a)))
            .then(a.key.cmp(&b.key))
    });
    rows
}

fn label_rows(rows: &mut [UsageRow], label: impl Fn(&str) -> Result<Option<String>>) -> Result<()> {
    for row in rows {
        if let Some(text) = label(&row.key)? {
            row.label = text;
        }
    }
    Ok(())
}

fn title(db: &Connection, session_id: &str) -> Result<Option<String>> {
    query_optional(db, "SELECT title FROM sessions WHERE id = ?1", [session_id])
}

/// By session: its primary repo's id and name.
fn primary_repos(db: &Connection) -> Result<HashMap<String, (String, String)>> {
    let mut statement = db.prepare_cached(
        "SELECT session_id, repo_id, name FROM session_repos WHERE is_primary = 1 \
         ORDER BY attached_seq DESC",
    )?;
    let mut primaries = HashMap::new();
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    // Oldest last, so the first primary a session had is the one kept.
    for row in rows {
        let (session_id, repo_id, name) = row?;
        primaries.insert(session_id, (repo_id, name));
    }
    Ok(primaries)
}
