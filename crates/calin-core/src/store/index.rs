//! The SQLite index: session and agent projections plus one row per event
//! pointing into the session's log, and what the graph and the timeline are
//! read from (`views.rs`). Everything here can be rebuilt from the logs.
//!
//! A projection row is derived from its own session's events only, in log
//! order. That is what makes a reindex give the same rows whichever order
//! the sessions are read in.

use std::path::Path;

use calin_proto::{
    Agent, AgentKind, AgentMessage, AgentStatus, Event, EventBody, ItemContent, NodePhase,
    NodeRunState, PullRequest, Session, SessionRepo, SessionStatus, WorkflowEdge, WorkflowGraph,
    WorkflowNodeRun, WorkflowRun, WorkflowStatus,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{OpenPermission, OpenQuestion};
use crate::error::Result;
use crate::title::excerpt;
use crate::workflows::loop_max;

/// Bumped when the tables change. A database with another version is
/// dropped and rebuilt from the logs, so there are no migrations to write.
const SCHEMA_VERSION: i64 = 5;

const SCHEMA: &str = "
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL,
    runner            TEXT NOT NULL,
    permission_mode   TEXT NOT NULL,
    -- The workflow the session was created with; NULL for the built-in `quick`.
    workflow          TEXT,
    branch            TEXT,
    parent_session_id TEXT,
    root_session_id   TEXT NOT NULL,
    head_seq          INTEGER NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    -- Bytes of the log that are indexed. A longer log has events to catch up on.
    log_len           INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX sessions_by_root ON sessions (root_session_id);

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
    spawned_seq     INTEGER NOT NULL,
    spawned_ts      INTEGER NOT NULL,
    -- What a subagent was asked to do: the title of the tool call that started it.
    label           TEXT
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
    -- For an event that can start a turn (a user message, a message that
    -- arrived, an answer): its first line, which titles the turn.
    label      TEXT,
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
    attached_seq INTEGER NOT NULL,
    attached_ts  INTEGER NOT NULL
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

-- The pull request of each repo of a session, as last recorded: one per
-- session repo, replaced when a newer event describes it.
CREATE TABLE pull_requests (
    session_repo_id TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    number          INTEGER NOT NULL,
    url             TEXT NOT NULL,
    title           TEXT NOT NULL,
    state           TEXT NOT NULL,
    checks          TEXT NOT NULL,
    review          TEXT NOT NULL,
    updated_at      INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX pull_requests_by_session ON pull_requests (session_id);

-- Questions put to the user and whether each was answered.
CREATE TABLE questions (
    session_id  TEXT NOT NULL,
    question_id TEXT NOT NULL,
    agent_id    TEXT,
    -- The `question_asked` event, the cause of the answer.
    event_id    TEXT NOT NULL,
    answered    INTEGER NOT NULL,
    PRIMARY KEY (session_id, question_id)
) WITHOUT ROWID;

-- The timeline's spans: turns, tool calls and waiting periods, each in the
-- lane of one agent. `end_ts` is NULL while the span is open.
CREATE TABLE spans (
    session_id TEXT NOT NULL,
    -- `<kind>:<id of the event that opened it>`: unique across sessions.
    id         TEXT NOT NULL,
    agent_id   TEXT NOT NULL,
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    tool_kind  TEXT,
    status     TEXT,
    -- For a tool span: the item, which is how its completion finds it.
    item_id    TEXT,
    start_ts   INTEGER NOT NULL,
    end_ts     INTEGER,
    seq        INTEGER NOT NULL,
    PRIMARY KEY (session_id, id)
) WITHOUT ROWID;
CREATE INDEX spans_open ON spans (session_id, kind, agent_id) WHERE end_ts IS NULL;
CREATE INDEX spans_by_item ON spans (session_id, item_id) WHERE item_id IS NOT NULL;

-- One row per `message_sent` and per `message_received`, in the session
-- whose log holds the event.
CREATE TABLE messages (
    session_id      TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    id              TEXT NOT NULL,
    sent            INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    from_session_id TEXT NOT NULL,
    from_agent_id   TEXT,
    to_session_id   TEXT NOT NULL,
    to_agent_id     TEXT,
    -- The first line of the body, cut to fit an edge.
    label           TEXT NOT NULL,
    ts              INTEGER NOT NULL,
    -- Ids sort in the order events were written, across sessions.
    event_id        TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE TABLE file_changes (
    session_id      TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    agent_id        TEXT,
    session_repo_id TEXT,
    path            TEXT NOT NULL,
    ts              INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

-- Every change of an agent's status or totals, so that the graph can say
-- what they were at any moment. A row sets the columns that are not NULL.
CREATE TABLE agent_states (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    agent_id   TEXT NOT NULL,
    ts         INTEGER NOT NULL,
    status     TEXT,
    cost_usd   REAL,
    tokens_in  INTEGER,
    tokens_out INTEGER,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE TABLE session_states (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    ts         INTEGER NOT NULL,
    status     TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

-- The workflow run of a session, from its `workflow_started` and
-- `workflow_finished` events (workflows spec, section 6).
CREATE TABLE workflow_runs (
    session_id  TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    name        TEXT NOT NULL,
    -- The graph as it was when the run started, as JSON.
    graph       TEXT NOT NULL,
    budget_usd  REAL,
    status      TEXT NOT NULL,
    reason      TEXT
) WITHOUT ROWID;

-- One row per node of the run's graph, kept current by `workflow_node` events.
CREATE TABLE workflow_nodes (
    session_id TEXT NOT NULL,
    -- Position in the graph, which is the order they are served in.
    position   INTEGER NOT NULL,
    node_id    TEXT NOT NULL,
    is_loop    INTEGER NOT NULL,
    -- A loop's limit.
    max        INTEGER,
    state      TEXT NOT NULL,
    runs       INTEGER NOT NULL,
    last_port  TEXT,
    outcome    TEXT,
    PRIMARY KEY (session_id, position)
) WITHOUT ROWID;
CREATE INDEX workflow_nodes_by_id ON workflow_nodes (session_id, node_id);

-- The edges a run went through, in order: one row per node that finished
-- through a wired port.
CREATE TABLE workflow_edges (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    from_node  TEXT NOT NULL,
    port       TEXT NOT NULL,
    to_node    TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
";

/// Every table, for dropping a database of another schema version.
const TABLES: [&str; 15] = [
    "workflow_edges",
    "workflow_nodes",
    "workflow_runs",
    "session_states",
    "agent_states",
    "file_changes",
    "messages",
    "spans",
    "questions",
    "pull_requests",
    "permissions",
    "session_repos",
    "events",
    "agents",
    "sessions",
];

/// The tables that hold what a session's events projected, keyed by `session_id`.
const PROJECTIONS: [&str; 14] = [
    "workflow_edges",
    "workflow_nodes",
    "workflow_runs",
    "session_states",
    "agent_states",
    "file_changes",
    "messages",
    "spans",
    "questions",
    "pull_requests",
    "permissions",
    "session_repos",
    "events",
    "agents",
];

const SESSION_COLUMNS: &str = "id, title, prompt, status, runner, branch, parent_session_id, \
     root_session_id, head_seq, created_at, updated_at, permission_mode, workflow";

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
         permission_mode, workflow, log_len) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)",
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
        session.workflow,
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

/// The session's pull requests, in the order of its repo set.
pub(super) fn pull_requests(db: &Connection, session_id: &str) -> Result<Vec<PullRequest>> {
    let mut statement = db.prepare_cached(
        "SELECT p.session_repo_id, p.number, p.url, p.title, p.state, p.checks, p.review, \
         p.updated_at FROM pull_requests p \
         LEFT JOIN session_repos r ON r.id = p.session_repo_id \
         WHERE p.session_id = ?1 \
         ORDER BY r.attached_seq IS NULL, r.attached_seq, p.session_repo_id",
    )?;
    let pull_requests = statement
        .query_map([session_id], |row| {
            Ok(PullRequest {
                session_repo_id: row.get(0)?,
                number: row.get(1)?,
                url: row.get(2)?,
                title: row.get(3)?,
                state: from_wire_name(row, 4)?,
                checks: from_wire_name(row, 5)?,
                review: from_wire_name(row, 6)?,
                updated_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(pull_requests)
}

/// The highest number among the pull requests recorded for `repo_id`, in any
/// session, whose URL starts with `url_prefix`. 0 when there is none.
pub(super) fn last_pull_request_number(
    db: &Connection,
    repo_id: &str,
    url_prefix: &str,
) -> Result<u32> {
    Ok(db
        .prepare_cached(
            "SELECT COALESCE(MAX(p.number), 0) FROM pull_requests p \
             JOIN session_repos r ON r.id = p.session_repo_id \
             WHERE r.repo_id = ?1 AND substr(p.url, 1, length(?2)) = ?2",
        )?
        .query_row([repo_id, url_prefix], |row| row.get(0))?)
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
        "INSERT INTO events (session_id, seq, id, agent_id, parent_id, kind, log_offset, ts, \
         label) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
        input_label(&event.body),
    ])?;

    match &event.body {
        EventBody::SessionStatus { status, reason } => {
            let name = wire_name(status)?;
            db.prepare_cached("UPDATE sessions SET status = ?1 WHERE id = ?2")?
                .execute(params![name, event.session_id])?;
            db.prepare_cached(
                "INSERT OR REPLACE INTO session_states (session_id, seq, ts, status) \
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![event.session_id, event.seq as i64, event.ts, name])?;
            // Whatever the session moved to, it is no longer the waiting it was in.
            db.prepare_cached(
                "UPDATE spans SET end_ts = ?1 \
                 WHERE session_id = ?2 AND kind = 'waiting' AND end_ts IS NULL",
            )?
            .execute(params![event.ts, event.session_id])?;
            if *status == SessionStatus::Waiting
                && let Some(lead) = lead_agent(db, &event.session_id)?
            {
                let title = match reason.as_deref() {
                    Some("permission") => "Waiting for permission",
                    Some("question") => "Waiting for an answer",
                    Some("gate") => "Waiting at a gate",
                    _ => "Waiting",
                };
                open_span(db, event, "wait", &lead, "waiting", title, event.ts)?;
            }
        }
        EventBody::AgentSpawned { agent } => {
            // A subagent is described by the tool call that started it.
            let label = match (agent.kind, &event.parent_id) {
                (AgentKind::Subagent, Some(started)) => {
                    span_title(db, &event.session_id, &format!("tool:{started}"))?
                }
                _ => None,
            };
            db.prepare_cached(
                "INSERT OR REPLACE INTO agents (id, session_id, parent_agent_id, kind, role, \
                 runner, model, status, cost_usd, tokens_in, tokens_out, spawned_seq, \
                 native_session_id, spawned_ts, label) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                event.ts,
                label,
            ])?;
            agent_state(db, event, &agent.id, Some(agent.status), None)?;
            if !ended(agent.status) {
                match agent.kind {
                    // Its one turn: from its start to its end.
                    AgentKind::Subagent => {
                        let title = label.unwrap_or_else(|| agent.role.clone());
                        open_turn(db, event, &agent.id, &title, event.ts)?;
                    }
                    AgentKind::Process => open_caused_turn(db, event, &agent.id)?,
                }
            }
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
                 path, branch, base_ref, is_primary, attached_seq, attached_ts) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                event.ts,
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
                agent_state(db, event, agent_id, Some(*status), None)?;
                // The lead's `running` opens a turn; any other status ends the
                // one in progress (the semantics of `TimelineSpan`).
                if *status == AgentStatus::Running {
                    open_caused_turn(db, event, agent_id)?;
                } else if *status != AgentStatus::Starting {
                    close_turn(db, event, agent_id)?;
                }
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
                let totals = (*cost_usd, *tokens_in, *tokens_out);
                agent_state(db, event, agent_id, None, Some(totals))?;
            }
        }
        // Opened or updated, the event carries the whole value: the newest wins.
        EventBody::PullRequestOpened { pull_request }
        | EventBody::PullRequestUpdated { pull_request } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO pull_requests (session_repo_id, session_id, number, \
                 url, title, state, checks, review, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute(params![
                pull_request.session_repo_id,
                event.session_id,
                pull_request.number,
                pull_request.url,
                pull_request.title,
                wire_name(&pull_request.state)?,
                wire_name(&pull_request.checks)?,
                wire_name(&pull_request.review)?,
                pull_request.updated_at,
            ])?;
        }
        // A tool call is a span in the lane of the agent that made it. The
        // orchestrator's own (a setup command) has no lane.
        EventBody::ItemStarted { item } => {
            if let (
                ItemContent::Tool {
                    title, tool_kind, ..
                },
                Some(agent_id),
            ) = (&item.content, &event.agent_id)
            {
                open_span(db, event, "tool", agent_id, "tool", title, event.ts)?;
                db.prepare_cached(
                    "UPDATE spans SET tool_kind = ?1, status = ?2, item_id = ?3 \
                     WHERE session_id = ?4 AND id = ?5",
                )?
                .execute(params![
                    wire_name(tool_kind)?,
                    wire_name(&item.status)?,
                    item.id,
                    event.session_id,
                    format!("tool:{}", event.id),
                ])?;
            }
        }
        EventBody::ItemCompleted { item } => {
            if let ItemContent::Tool { title, .. } = &item.content {
                db.prepare_cached(
                    "UPDATE spans SET end_ts = ?1, status = ?2, title = ?3 \
                     WHERE session_id = ?4 AND item_id = ?5 AND end_ts IS NULL",
                )?
                .execute(params![
                    event.ts,
                    wire_name(&item.status)?,
                    title,
                    event.session_id,
                    item.id,
                ])?;
            }
        }
        EventBody::MessageSent { message } => index_message(db, event, message, true)?,
        EventBody::MessageReceived { message } => index_message(db, event, message, false)?,
        EventBody::QuestionAsked { question } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO questions (session_id, question_id, agent_id, \
                 event_id, answered) VALUES (?1, ?2, ?3, ?4, 0)",
            )?
            .execute(params![
                event.session_id,
                question.id,
                event.agent_id,
                event.id
            ])?;
        }
        EventBody::QuestionAnswered { question_id, .. } => {
            db.prepare_cached(
                "UPDATE questions SET answered = 1 WHERE session_id = ?1 AND question_id = ?2",
            )?
            .execute(params![event.session_id, question_id])?;
        }
        EventBody::FileChanged {
            session_repo_id,
            path,
            ..
        } => {
            db.prepare_cached(
                "INSERT OR REPLACE INTO file_changes (session_id, seq, agent_id, \
                 session_repo_id, path, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                event.session_id,
                event.seq as i64,
                event.agent_id,
                session_repo_id,
                path,
                event.ts,
            ])?;
        }
        EventBody::WorkflowStarted {
            workflow_id,
            name,
            graph,
            budget_usd,
        } => {
            // A session runs one workflow; a second start would replace it whole.
            for table in ["workflow_nodes", "workflow_edges"] {
                db.prepare_cached(&format!("DELETE FROM {table} WHERE session_id = ?1"))?
                    .execute([&event.session_id])?;
            }
            db.prepare_cached(
                "INSERT OR REPLACE INTO workflow_runs (session_id, workflow_id, name, graph, \
                 budget_usd, status, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            )?
            .execute(params![
                event.session_id,
                workflow_id,
                name,
                serde_json::to_string(graph)?,
                budget_usd,
                wire_name(&WorkflowStatus::Running)?,
            ])?;
            let mut insert = db.prepare_cached(
                "INSERT INTO workflow_nodes (session_id, position, node_id, is_loop, max, \
                 state, runs) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            )?;
            for (position, node) in graph.nodes.iter().enumerate() {
                insert.execute(params![
                    event.session_id,
                    position as i64,
                    node.id,
                    node.kind == "loop",
                    loop_max(node),
                    wire_name(&NodeRunState::Idle)?,
                ])?;
            }
        }
        EventBody::WorkflowNode {
            node_id,
            phase: NodePhase::Started,
            ..
        } => {
            db.prepare_cached(
                "UPDATE workflow_nodes SET state = ?1, runs = runs + 1 \
                 WHERE session_id = ?2 AND node_id = ?3",
            )?
            .execute(params![
                wire_name(&NodeRunState::Active)?,
                event.session_id,
                node_id
            ])?;
        }
        EventBody::WorkflowNode {
            node_id,
            phase: NodePhase::Finished,
            port,
            outcome,
        } => {
            // A node cut short finishes without a port and keeps its last one.
            db.prepare_cached(
                "UPDATE workflow_nodes SET state = ?1, last_port = COALESCE(?2, last_port), \
                 outcome = ?3 WHERE session_id = ?4 AND node_id = ?5",
            )?
            .execute(params![
                wire_name(&NodeRunState::Finished)?,
                port,
                outcome,
                event.session_id,
                node_id
            ])?;
            let edge = match port {
                Some(port) => workflow_graph(db, &event.session_id)?.and_then(|graph| {
                    let wired = |edge: &WorkflowEdge| edge.from == *node_id && edge.port == *port;
                    graph.edges.into_iter().find(wired)
                }),
                None => None,
            };
            if let Some(edge) = edge {
                db.prepare_cached(
                    "INSERT OR REPLACE INTO workflow_edges (session_id, seq, from_node, port, \
                     to_node) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![
                    event.session_id,
                    event.seq as i64,
                    edge.from,
                    edge.port,
                    edge.to
                ])?;
            }
        }
        EventBody::WorkflowFinished { status, reason } => {
            db.prepare_cached(
                "UPDATE workflow_runs SET status = ?1, reason = ?2 WHERE session_id = ?3",
            )?
            .execute(params![wire_name(status)?, reason, event.session_id])?;
            // Nothing is at work in a run that is over.
            db.prepare_cached(
                "UPDATE workflow_nodes SET state = ?1 WHERE session_id = ?2 AND state = ?3",
            )?
            .execute(params![
                wire_name(&NodeRunState::Finished)?,
                event.session_id,
                wire_name(&NodeRunState::Active)?
            ])?;
        }
        EventBody::UserMessage { .. }
        | EventBody::ItemDelta { .. }
        | EventBody::PlanUpdated { .. }
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

/// The first line of an event that can start a turn; `None` for any other.
fn input_label(body: &EventBody) -> Option<String> {
    let text = match body {
        EventBody::UserMessage { text } => text,
        EventBody::MessageReceived { message } => &message.body,
        EventBody::QuestionAnswered { answer, .. } => answer,
        // A workflow's agent node starts a turn on its prompt, whose first
        // line is what the node says it started with.
        EventBody::WorkflowNode {
            phase: NodePhase::Started,
            outcome: Some(line),
            ..
        } => line,
        _ => return None,
    };
    Some(excerpt(text)).filter(|label| !label.is_empty())
}

fn ended(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Done | AgentStatus::Failed | AgentStatus::Stopped
    )
}

/// The session's lead: its first agent with a process of its own.
fn lead_agent(db: &Connection, session_id: &str) -> Result<Option<String>> {
    Ok(db
        .prepare_cached(
            "SELECT id FROM agents WHERE session_id = ?1 AND kind = 'process' \
             ORDER BY spawned_seq, id LIMIT 1",
        )?
        .query_row([session_id], |row| row.get(0))
        .optional()?)
}

fn span_title(db: &Connection, session_id: &str, span_id: &str) -> Result<Option<String>> {
    Ok(db
        .prepare_cached("SELECT title FROM spans WHERE session_id = ?1 AND id = ?2")?
        .query_row([session_id, span_id], |row| row.get(0))
        .optional()?)
}

/// Opens a span named after the event that opens it.
fn open_span(
    db: &Connection,
    event: &Event,
    prefix: &str,
    agent_id: &str,
    kind: &str,
    title: &str,
    start_ts: i64,
) -> Result<()> {
    db.prepare_cached(
        "INSERT OR REPLACE INTO spans (session_id, id, agent_id, kind, title, start_ts, seq) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        event.session_id,
        format!("{prefix}:{}", event.id),
        agent_id,
        kind,
        title,
        start_ts,
        event.seq as i64,
    ])?;
    Ok(())
}

/// Opens a turn of `agent_id` at `start_ts`, or where its previous turn
/// ended when that is later: the input of a turn may have waited for it.
fn open_turn(
    db: &Connection,
    event: &Event,
    agent_id: &str,
    title: &str,
    start_ts: i64,
) -> Result<()> {
    // A turn the log never closed ends where the next one begins.
    close_turn(db, event, agent_id)?;
    let previous_end: Option<i64> = db
        .prepare_cached(
            "SELECT MAX(end_ts) FROM spans \
             WHERE session_id = ?1 AND id >= 'turn:' AND id < 'turn;' AND agent_id = ?2",
        )?
        .query_row([&event.session_id, agent_id], |row| row.get(0))?;
    let start_ts = start_ts.max(previous_end.unwrap_or(start_ts)).min(event.ts);
    open_span(db, event, "turn", agent_id, "turn", title, start_ts)
}

/// Opens the turn this event starts for a process agent: from the input
/// that caused it (the event's parent), titled with that input's first line.
fn open_caused_turn(db: &Connection, event: &Event, agent_id: &str) -> Result<()> {
    let cause: Option<(i64, Option<String>)> = match &event.parent_id {
        Some(parent) => db
            .prepare_cached("SELECT ts, label FROM events WHERE id = ?1 AND session_id = ?2")?
            .query_row([parent, &event.session_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?,
        None => None,
    };
    let (start_ts, label) = cause.unwrap_or((event.ts, None));
    let title = label.unwrap_or_else(|| "Turn".to_owned());
    open_turn(db, event, agent_id, &title, start_ts)
}

/// Ends the agent's turn, and the tool calls it never completed with it.
fn close_turn(db: &Connection, event: &Event, agent_id: &str) -> Result<()> {
    db.prepare_cached(
        "UPDATE spans SET end_ts = ?1 \
         WHERE session_id = ?2 AND kind = 'turn' AND agent_id = ?3 AND end_ts IS NULL",
    )?
    .execute(params![event.ts, event.session_id, agent_id])?;
    db.prepare_cached(
        "UPDATE spans SET end_ts = ?1, \
         status = CASE status WHEN 'running' THEN 'failed' ELSE status END \
         WHERE session_id = ?2 AND kind = 'tool' AND agent_id = ?3 AND end_ts IS NULL",
    )?
    .execute(params![event.ts, event.session_id, agent_id])?;
    Ok(())
}

/// Records what changed about an agent with this event.
fn agent_state(
    db: &Connection,
    event: &Event,
    agent_id: &str,
    status: Option<AgentStatus>,
    totals: Option<(f64, u64, u64)>,
) -> Result<()> {
    let status = status.as_ref().map(wire_name).transpose()?;
    db.prepare_cached(
        "INSERT OR REPLACE INTO agent_states (session_id, seq, agent_id, ts, status, cost_usd, \
         tokens_in, tokens_out) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        event.session_id,
        event.seq as i64,
        agent_id,
        event.ts,
        status,
        totals.map(|totals| totals.0),
        totals.map(|totals| totals.1 as i64),
        totals.map(|totals| totals.2 as i64),
    ])?;
    Ok(())
}

fn index_message(db: &Connection, event: &Event, message: &AgentMessage, sent: bool) -> Result<()> {
    db.prepare_cached(
        "INSERT OR REPLACE INTO messages (session_id, seq, id, sent, kind, from_session_id, \
         from_agent_id, to_session_id, to_agent_id, label, ts, event_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute(params![
        event.session_id,
        event.seq as i64,
        message.id,
        sent,
        wire_name(&message.kind)?,
        message.from_session_id,
        message.from_agent_id,
        message.to_session_id,
        message.to_agent_id,
        excerpt(&message.body),
        event.ts,
        event.id,
    ])?;
    Ok(())
}

/// Whether a question was answered; `None` when there is no such question.
pub(super) fn question_answered(
    db: &Connection,
    session_id: &str,
    question_id: &str,
) -> Result<Option<bool>> {
    Ok(db
        .prepare_cached(
            "SELECT answered FROM questions WHERE session_id = ?1 AND question_id = ?2",
        )?
        .query_row([session_id, question_id], |row| row.get(0))
        .optional()?)
}

/// The questions nobody answered yet, of one session or of all of them, oldest first.
pub(super) fn open_questions(
    db: &Connection,
    session_id: Option<&str>,
) -> Result<Vec<OpenQuestion>> {
    let mut statement = db.prepare_cached(
        "SELECT session_id, question_id, agent_id, event_id FROM questions \
         WHERE answered = 0 AND (?1 IS NULL OR session_id = ?1) ORDER BY event_id",
    )?;
    let open = statement
        .query_map([session_id], |row| {
            Ok(OpenQuestion {
                session_id: row.get(0)?,
                question_id: row.get(1)?,
                agent_id: row.get(2)?,
                event_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(open)
}

/// The graph a session's run started with; `None` when it runs no workflow.
fn workflow_graph(db: &Connection, session_id: &str) -> Result<Option<WorkflowGraph>> {
    let json: Option<String> = db
        .prepare_cached("SELECT graph FROM workflow_runs WHERE session_id = ?1")?
        .query_row([session_id], |row| row.get(0))
        .optional()?;
    Ok(match json {
        Some(json) => Some(serde_json::from_str(&json)?),
        None => None,
    })
}

/// What every agent of the tree `session_id` belongs to has cost so far.
pub(super) fn tree_cost(db: &Connection, session_id: &str) -> Result<f64> {
    Ok(db
        .prepare_cached(
            "SELECT COALESCE(SUM(a.cost_usd), 0.0) FROM agents a \
             JOIN sessions s ON s.id = a.session_id \
             WHERE s.root_session_id = (SELECT root_session_id FROM sessions WHERE id = ?1)",
        )?
        .query_row([session_id], |row| row.get(0))?)
}

/// The session's workflow run as its events left it, with what the tree
/// has cost. `None` for a session without a run.
pub(super) fn workflow_run(db: &Connection, session_id: &str) -> Result<Option<WorkflowRun>> {
    let run = db
        .prepare_cached(
            "SELECT workflow_id, name, graph, budget_usd, status, reason FROM workflow_runs \
             WHERE session_id = ?1",
        )?
        .query_row([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<f64>>(3)?,
                from_wire_name::<WorkflowStatus>(row, 4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()?;
    let Some((workflow_id, name, graph, budget_usd, status, reason)) = run else {
        return Ok(None);
    };
    let mut statement = db.prepare_cached(
        "SELECT node_id, state, runs, last_port, outcome, is_loop, max FROM workflow_nodes \
         WHERE session_id = ?1 ORDER BY position",
    )?;
    let nodes = statement
        .query_map([session_id], |row| {
            let runs: u32 = row.get(2)?;
            let max: Option<u32> = row.get(6)?;
            // A loop counts its entries up to its limit (spec section 3).
            let iteration = row
                .get::<_, bool>(5)?
                .then(|| max.map_or(runs, |max| runs.min(max)));
            Ok(WorkflowNodeRun {
                id: row.get(0)?,
                state: from_wire_name(row, 1)?,
                runs,
                last_port: row.get(3)?,
                outcome: row.get(4)?,
                iteration,
                max,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut statement = db.prepare_cached(
        "SELECT from_node, port, to_node FROM workflow_edges WHERE session_id = ?1 ORDER BY seq",
    )?;
    let taken = statement
        .query_map([session_id], |row| {
            Ok(WorkflowEdge {
                from: row.get(0)?,
                port: row.get(1)?,
                to: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(WorkflowRun {
        workflow_id,
        name,
        graph: serde_json::from_str(&graph)?,
        nodes,
        taken,
        status,
        reason,
        budget_usd,
        cost_usd: tree_cost(db, session_id)?,
    }))
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
        EventBody::PullRequestOpened { .. } => "pull_request_opened",
        EventBody::PullRequestUpdated { .. } => "pull_request_updated",
        EventBody::MessageSent { .. } => "message_sent",
        EventBody::MessageReceived { .. } => "message_received",
        EventBody::QuestionAsked { .. } => "question_asked",
        EventBody::QuestionAnswered { .. } => "question_answered",
        EventBody::WorkflowStarted { .. } => "workflow_started",
        EventBody::WorkflowNode { .. } => "workflow_node",
        EventBody::WorkflowFinished { .. } => "workflow_finished",
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

pub(super) fn from_wire_name<T: DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let name: String = row.get(index)?;
    serde_json::from_value(serde_json::Value::String(name))
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(err)))
}

pub(super) fn uint(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
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
        workflow: row.get(12)?,
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

    fn pull_request() -> PullRequest {
        PullRequest {
            session_repo_id: "r".into(),
            number: 1,
            url: "https://example.invalid/x/pull/1".into(),
            title: "t".into(),
            state: calin_proto::PullRequestState::Draft,
            checks: calin_proto::CheckState::None,
            review: calin_proto::ReviewState::None,
            updated_at: 0,
        }
    }

    fn note() -> AgentMessage {
        AgentMessage {
            id: "m".into(),
            kind: calin_proto::MessageKind::Note,
            from_session_id: "s".into(),
            from_agent_id: None,
            to_session_id: "s".into(),
            to_agent_id: None,
            body: "b".into(),
            in_reply_to: None,
            ts: 0,
        }
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
            EventBody::PullRequestOpened {
                pull_request: pull_request(),
            },
            EventBody::PullRequestUpdated {
                pull_request: pull_request(),
            },
            EventBody::MessageSent { message: note() },
            EventBody::MessageReceived { message: note() },
            EventBody::QuestionAsked {
                question: calin_proto::Question {
                    id: "q".into(),
                    text: "?".into(),
                    options: vec![],
                    gate: false,
                },
            },
            EventBody::QuestionAnswered {
                question_id: "q".into(),
                answer: String::new(),
                by: calin_proto::DecisionSource::Expired,
            },
            EventBody::WorkflowStarted {
                workflow_id: "w".into(),
                name: "W".into(),
                graph: WorkflowGraph {
                    nodes: vec![],
                    edges: vec![],
                },
                budget_usd: None,
            },
            EventBody::WorkflowNode {
                node_id: "n".into(),
                phase: NodePhase::Started,
                port: None,
                outcome: None,
            },
            EventBody::WorkflowFinished {
                status: WorkflowStatus::Success,
                reason: None,
            },
        ];
        for body in bodies {
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(json["type"], kind(&body));
        }
    }
}
