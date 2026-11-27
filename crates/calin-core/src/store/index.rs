//! The SQLite index: session and agent projections plus one row per event
//! pointing into the session's log. Everything here can be rebuilt from the logs.

use std::path::Path;

use calin_proto::{Agent, Event, EventBody, Session, SessionRepo};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::OpenPermission;
use crate::error::Result;

/// Bumped when the tables change. A database with another version is
/// dropped and rebuilt from the logs, so there are no migrations to write.
const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL,
    runner            TEXT NOT NULL,
    permission_mode   TEXT NOT NULL,
    branch            TEXT,
    parent_session_id TEXT,
    root_session_id   TEXT NOT NULL,
    head_seq          INTEGER NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    -- Bytes of the log that are indexed. A longer log has events to catch up on.
    log_len           INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE agents (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    parent_agent_id TEXT,
    kind            TEXT NOT NULL,
    role            TEXT NOT NULL,
    runner          TEXT NOT NULL,
    model           TEXT,
    native_session_id TEXT,
    status          TEXT NOT NULL,
    cost_usd        REAL NOT NULL,
    tokens_in       INTEGER NOT NULL,
    tokens_out      INTEGER NOT NULL,
    spawned_seq     INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX agents_by_session ON agents (session_id, spawned_seq);

CREATE TABLE events (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    id         TEXT NOT NULL,
    agent_id   TEXT,
    parent_id  TEXT,
    kind       TEXT NOT NULL,
    log_offset INTEGER NOT NULL,
    ts         INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
CREATE UNIQUE INDEX events_by_id ON events (id);

-- The repo set, learned from `repo_attached` events.
CREATE TABLE session_repos (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL,
    repo_id      TEXT NOT NULL,
    name         TEXT NOT NULL,
    mode         TEXT NOT NULL,
    path         TEXT NOT NULL,
    branch       TEXT,
    base_ref     TEXT,
    is_primary   INTEGER NOT NULL,
    attached_seq INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX session_repos_by_session ON session_repos (session_id, attached_seq);
CREATE INDEX session_repos_by_repo ON session_repos (repo_id);

-- Permission requests and whether each was answered.
CREATE TABLE permissions (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    agent_id   TEXT,
    -- The `permission_requested` event, the cause of the answer.
    event_id   TEXT NOT NULL,
    resolved   INTEGER NOT NULL,
    PRIMARY KEY (session_id, request_id)
) WITHOUT ROWID;
CREATE INDEX permissions_open ON permissions (resolved, session_id);
";

/// Every table, for dropping a database of another schema version.
const TABLES: [&str; 5] = [
    "permissions",
    "session_repos",
    "events",
    "agents",
    "sessions",
];

/// The tables that hold what a session's events projected, keyed by `session_id`.
const PROJECTIONS: [&str; 4] = ["permissions", "session_repos", "events", "agents"];

const SESSION_COLUMNS: &str = "id, title, prompt, status, runner, branch, parent_session_id, \
     root_session_id, head_seq, created_at, updated_at, permission_mode";

const AGENT_COLUMNS: &str = "id, session_id, parent_agent_id, kind, role, runner, model, status, \
     cost_usd, tokens_in, tokens_out, native_session_id";

const REPO_COLUMNS: &str =
    "id, session_id, repo_id, name, mode, path, branch, base_ref, is_primary";

pub(super) fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    // With WAL, NORMAL cannot corrupt the database; a power cut may drop the
    // newest index rows, and those are caught up from the logs on startup.
    db.pragma_update(None, "synchronous", "NORMAL")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        for table in TABLES {
            db.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
        }
        db.execute_batch(SCHEMA)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(db)
}

/// Where the index stands for one session.
pub(super) struct Indexed {
    pub head_seq: u64,
    pub log_len: u64,
}

pub(super) fn indexed(db: &Connection, session_id: &str) -> Result<Option<Indexed>> {
    Ok(db
        .prepare_cached("SELECT head_seq, log_len FROM sessions WHERE id = ?1")?
        .query_row([session_id], |row| {
            Ok(Indexed {
                head_seq: uint(row, 0)?,
                log_len: uint(row, 1)?,
            })
        })
        .optional()?)
}

pub(super) fn session_ids(db: &Connection) -> Result<Vec<String>> {
    let mut statement = db.prepare("SELECT id FROM sessions")?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Inserts the session with nothing indexed yet, replacing any earlier row.
pub(super) fn insert_session(db: &Connection, session: &Session) -> Result<()> {
    db.prepare_cached(
        "INSERT OR REPLACE INTO sessions (id, title, prompt, status, runner, branch, \
         parent_session_id, root_session_id, head_seq, created_at, updated_at, \
         permission_mode, log_len) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)",
    )?
    .execute(params![
        session.id,
        session.title,
        session.prompt,
        wire_name(&session.status)?,
        session.runner,
        session.branch,
        session.parent_session_id,
        session.root_session_id,
        session.head_seq as i64,
        session.created_at,
        session.updated_at,
        wire_name(&session.permission_mode)?,
    ])?;
    Ok(())
}

/// Forgets a session. The log, if any, is left alone.
pub(super) fn delete_session(db: &Connection, session_id: &str) -> Result<()> {
    delete_projections(db, session_id)?;
    db.execute("DELETE FROM sessions WHERE id = ?1", [session_id])?;
    Ok(())
}

fn delete_projections(db: &Connection, session_id: &str) -> Result<()> {
    for table in PROJECTIONS {
        db.execute(
            &format!("DELETE FROM {table} WHERE session_id = ?1"),
            [session_id],
        )?;
    }
    Ok(())
}

/// Returns a session to its state before any event, keeping what it was created with.
pub(super) fn reset_session(db: &Connection, session_id: &str) -> Result<()> {
    delete_projections(db, session_id)?;
    db.execute(
        "UPDATE sessions SET status = 'queued', branch = NULL, head_seq = 0, log_len = 0, \
         updated_at = created_at WHERE id = ?1",
        [session_id],
    )?;
    Ok(())
}

pub(super) fn session(db: &Connection, session_id: &str) -> Result<Option<Session>> {
    Ok(db
        .prepare_cached(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"
        ))?
        .query_row([session_id], session_from_row)
        .optional()?)
}

/// Every session, newest first.
pub(super) fn sessions(db: &Connection) -> Result<Vec<Session>> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY created_at DESC, id DESC"
    ))?;
    let sessions = statement
        .query_map([], session_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(sessions)
}

/// The agents of a session in the order they were spawned.
pub(super) fn agents(db: &Connection, session_id: &str) -> Result<Vec<Agent>> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {AGENT_COLUMNS} FROM agents WHERE session_id = ?1 ORDER BY spawned_seq, id"
    ))?;
    let agents = statement
        .query_map([session_id], agent_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(agents)
}

/// The repo set of a session in the order the repos were attached, which
/// puts the primary repo first.
pub(super) fn session_repos(db: &Connection, session_id: &str) -> Result<Vec<SessionRepo>> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {REPO_COLUMNS} FROM session_repos WHERE session_id = ?1 \
         ORDER BY attached_seq, id"
    ))?;
    let repos = statement
        .query_map([session_id], repo_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(repos)
}

/// True when the repo is attached to a session that may still work in it.
pub(super) fn repo_in_use(db: &Connection, repo_id: &str) -> Result<bool> {
    Ok(db
        .prepare_cached(
            "SELECT 1 FROM session_repos r JOIN sessions s ON s.id = r.session_id \
             WHERE r.repo_id = ?1 AND s.status NOT IN ('done', 'failed', 'cancelled') LIMIT 1",
        )?
        .query_row([repo_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Whether a permission request was answered; `None` when there is no such request.
pub(super) fn permission_resolved(
    db: &Connection,
    session_id: &str,
    request_id: &str,
) -> Result<Option<bool>> {
    Ok(db
        .prepare_cached(
            "SELECT resolved FROM permissions WHERE session_id = ?1 AND request_id = ?2",
        )?
        .query_row([session_id, request_id], |row| row.get(0))
        .optional()?)
}

/// The requests nobody answered yet, of one session or of all of them, oldest first.
pub(super) fn open_permissions(
    db: &Connection,
    session_id: Option<&str>,
) -> Result<Vec<OpenPermission>> {
    let mut statement = db.prepare_cached(
        "SELECT session_id, request_id, agent_id, event_id FROM permissions \
         WHERE resolved = 0 AND (?1 IS NULL OR session_id = ?1) ORDER BY event_id",
    )?;
    let open = statement
        .query_map([session_id], |row| {
            Ok(OpenPermission {
                session_id: row.get(0)?,
                request_id: row.get(1)?,
                agent_id: row.get(2)?,
                event_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(open)
}

/// Byte offset of an event's line in its session log.
pub(super) fn log_offset(db: &Connection, session_id: &str, seq: u64) -> Result<Option<u64>> {
    Ok(db
        .prepare_cached("SELECT log_offset FROM events WHERE session_id = ?1 AND seq = ?2")?
        .query_row(params![session_id, seq as i64], |row| uint(row, 0))
        .optional()?)
}

/// Indexes one logged event and brings the projections up to date.
/// `offset..end` is the event's line in the log. Call inside a transaction.
pub(super) fn index_event(db: &Connection, event: &Event, offset: u64, end: u64) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO events (session_id, seq, id, agent_id, parent_id, kind, log_offset, ts) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        event.session_id,
        event.seq as i64,
        event.id,
        event.agent_id,
        event.parent_id,
        kind(&event.body),
        offset as i64,
        event.ts,
    ])?;

    match &event.body {
        EventBody::SessionStatus { status, .. } => {
            db.prepare_cached("UPDATE sessions SET status = ?1 WHERE id = ?2")?
                .execute(params![wire_name(status)?, event.session_id])?;
        }
        EventBody::AgentSpawned { agent } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO agents (id, session_id, parent_agent_id, kind, role, \
                 runner, model, status, cost_usd, tokens_in, tokens_out, spawned_seq, \
                 native_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?
            .execute(params![
                agent.id,
                event.session_id,
                agent.parent_agent_id,
                wire_name(&agent.kind)?,
                agent.role,
                agent.runner,
                agent.model,
                wire_name(&agent.status)?,
                agent.cost_usd,
                agent.tokens_in as i64,
                agent.tokens_out as i64,
                event.seq as i64,
                agent.native_session_id,
            ])?;
        }
        EventBody::AgentSession {
            native_session_id,
            model,
        } => {
            if let Some(agent_id) = &event.agent_id {
                // A report without a model keeps the one already known.
                db.prepare_cached(
                    "UPDATE agents SET native_session_id = ?1, model = COALESCE(?2, model) \
                     WHERE id = ?3 AND session_id = ?4",
                )?
                .execute(params![
                    native_session_id,
                    model,
                    agent_id,
                    event.session_id
                ])?;
            }
        }
        EventBody::RepoAttached { repo } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO session_repos (id, session_id, repo_id, name, mode, \
                 path, branch, base_ref, is_primary, attached_seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                repo.id,
                event.session_id,
                repo.repo_id,
                repo.name,
                wire_name(&repo.mode)?,
                repo.path,
                repo.branch,
                repo.base_ref,
                repo.primary,
                event.seq as i64,
            ])?;
            // The session's branch is the one of the first repo that has one.
            db.prepare_cached("UPDATE sessions SET branch = ?1 WHERE id = ?2 AND branch IS NULL")?
                .execute(params![repo.branch, event.session_id])?;
        }
        EventBody::PermissionRequested { request } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO permissions (session_id, request_id, agent_id, \
                 event_id, resolved) VALUES (?1, ?2, ?3, ?4, 0)",
            )?
            .execute(params![
                event.session_id,
                request.id,
                event.agent_id,
                event.id
            ])?;
        }
        EventBody::PermissionResolved { request_id, .. } => {
            db.prepare_cached(
                "UPDATE permissions SET resolved = 1 WHERE session_id = ?1 AND request_id = ?2",
            )?
            .execute(params![event.session_id, request_id])?;
        }
        EventBody::AgentStatus { status } => {
            if let Some(agent_id) = &event.agent_id {
                db.prepare_cached(
                    "UPDATE agents SET status = ?1 WHERE id = ?2 AND session_id = ?3",
                )?
                .execute(params![
                    wire_name(status)?,
                    agent_id,
                    event.session_id
                ])?;
            }
        }
        EventBody::Usage {
            cost_usd,
            tokens_in,
            tokens_out,
        } => {
            if let Some(agent_id) = &event.agent_id {
                db.prepare_cached(
                    "UPDATE agents SET cost_usd = ?1, tokens_in = ?2, tokens_out = ?3 \
                     WHERE id = ?4 AND session_id = ?5",
                )?
                .execute(params![
                    cost_usd,
                    *tokens_in as i64,
                    *tokens_out as i64,
                    agent_id,
                    event.session_id
                ])?;
            }
        }
        EventBody::UserMessage { .. }
        | EventBody::ItemStarted { .. }
        | EventBody::ItemDelta { .. }
        | EventBody::ItemCompleted { .. }
        | EventBody::PlanUpdated { .. }
        | EventBody::FileChanged { .. }
        | EventBody::Error { .. } => {}
    }

    db.prepare_cached(
        "UPDATE sessions SET head_seq = ?1, updated_at = ?2, log_len = ?3 WHERE id = ?4",
    )?
    .execute(params![
        event.seq as i64,
        event.ts,
        end as i64,
        event.session_id
    ])?;
    Ok(())
}

/// The event's `type` tag, as it appears on the wire.
fn kind(body: &EventBody) -> &'static str {
    match body {
        EventBody::SessionStatus { .. } => "session_status",
        EventBody::UserMessage { .. } => "user_message",
        EventBody::AgentSpawned { .. } => "agent_spawned",
        EventBody::AgentStatus { .. } => "agent_status",
        EventBody::AgentSession { .. } => "agent_session",
        EventBody::ItemStarted { .. } => "item_started",
        EventBody::ItemDelta { .. } => "item_delta",
        EventBody::ItemCompleted { .. } => "item_completed",
        EventBody::PlanUpdated { .. } => "plan_updated",
        EventBody::Usage { .. } => "usage",
        EventBody::RepoAttached { .. } => "repo_attached",
        EventBody::PermissionRequested { .. } => "permission_requested",
        EventBody::PermissionResolved { .. } => "permission_resolved",
        EventBody::FileChanged { .. } => "file_changed",
        EventBody::Error { .. } => "error",
    }
}

/// A unit enum's wire name, so the index and the log spell statuses alike.
fn wire_name<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(name) => Ok(name),
        other => Ok(other.to_string()),
    }
}

fn from_wire_name<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let name: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(name))
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(err)))
}

fn uint(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        title: row.get(1)?,
        prompt: row.get(2)?,
        status: from_wire_name(row, 3)?,
        runner: row.get(4)?,
        branch: row.get(5)?,
        parent_session_id: row.get(6)?,
        root_session_id: row.get(7)?,
        head_seq: uint(row, 8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        permission_mode: from_wire_name(row, 11)?,
    })
}

fn agent_from_row(row: &Row<'_>) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: row.get(0)?,
        session_id: row.get(1)?,
        parent_agent_id: row.get(2)?,
        kind: from_wire_name(row, 3)?,
        role: row.get(4)?,
        runner: row.get(5)?,
        model: row.get(6)?,
        status: from_wire_name(row, 7)?,
        cost_usd: row.get(8)?,
        tokens_in: uint(row, 9)?,
        tokens_out: uint(row, 10)?,
        native_session_id: row.get(11)?,
    })
}

fn repo_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRepo> {
    Ok(SessionRepo {
        id: row.get(0)?,
        session_id: row.get(1)?,
        repo_id: row.get(2)?,
        name: row.get(3)?,
        mode: from_wire_name(row, 4)?,
        path: row.get(5)?,
        branch: row.get(6)?,
        base_ref: row.get(7)?,
        primary: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use calin_proto::{AgentStatus, SessionStatus};

    use super::*;

    #[test]
    fn enums_are_stored_under_their_wire_names() {
        assert_eq!(wire_name(&SessionStatus::Review).unwrap(), "review");
        assert_eq!(wire_name(&AgentStatus::Idle).unwrap(), "idle");
    }

    #[test]
    fn the_kind_column_matches_the_wire_tag() {
        let bodies = [
            EventBody::UserMessage { text: "hi".into() },
            EventBody::AgentStatus {
                status: AgentStatus::Idle,
            },
            EventBody::PlanUpdated { entries: vec![] },
            EventBody::Usage {
                cost_usd: 0.0,
                tokens_in: 0,
                tokens_out: 0,
            },
            EventBody::Error {
                message: "x".into(),
            },
            EventBody::AgentSession {
                native_session_id: "n".into(),
                model: None,
            },
            EventBody::PermissionResolved {
                request_id: "r".into(),
                decision: calin_proto::PermissionDecision::Deny,
                by: calin_proto::DecisionSource::Expired,
                message: None,
            },
            EventBody::FileChanged {
                session_repo_id: None,
                path: "/x".into(),
                op: calin_proto::FileOp::Edit,
            },
        ];
        for body in bodies {
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(json["type"], kind(&body));
        }
    }
}
