//! The graph and the timeline of a session tree, read from the index alone
//! (the semantics in `calin-proto/src/graph.rs`). Nothing here reads a log:
//! every row was projected when its event was indexed, so a request costs a
//! few small queries per session of the tree, however long the logs are.

use std::collections::{BTreeMap, HashMap, HashSet};

use calin_proto::{
    Agent, AgentKind, AgentStatus, EdgeKind, Graph, GraphEdge, GraphNode, MessageKind, NodeKind,
    RepoMode, SessionStatus, Timeline, TimelineArrow, TimelineLane, TimelineSpan,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::index::{
    agent_from_row, from_optional_wire_name, from_wire_name, query_all, query_optional, uint,
};
use crate::error::Result;

const USER_NODE: &str = "user";
const USER_LABEL: &str = "You";

fn agent_node(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}

fn repo_node(repo_id: &str) -> String {
    format!("repo:{repo_id}")
}

/// The node of a message's sender or recipient: an agent, or the user.
fn participant(agent_id: Option<&str>) -> String {
    agent_id.map_or_else(|| USER_NODE.to_owned(), agent_node)
}

struct TreeSession {
    id: String,
    title: String,
    parent_session_id: Option<String>,
}

/// The sessions that share a root, oldest first, and the time they span.
struct Tree {
    root: String,
    sessions: Vec<TreeSession>,
    from_ts: i64,
    to_ts: i64,
}

struct TreeAgent {
    agent: Agent,
    spawned_ts: i64,
    /// The `agent_spawned` event. Event ids sort in the order they were
    /// written, which orders what happened within one millisecond.
    spawned_event: String,
    /// What a subagent was asked to do.
    label: Option<String>,
}

/// A message as its sender's log has it.
struct Sent {
    id: String,
    kind: MessageKind,
    from_agent_id: Option<String>,
    to_session_id: String,
    to_agent_id: Option<String>,
    label: String,
    ts: i64,
    /// The `message_sent` event.
    event_id: String,
}

/// An agent in spawn-tree order, with the agent it hangs under: `None` is the user.
type Ordered = (TreeAgent, Option<String>);

/// A session of the tree, with its `updated_at` and its `head_seq`.
type TreeRow = (TreeSession, i64, u64);

fn tree(db: &Connection, session_id: &str) -> Result<Option<Tree>> {
    let root: Option<(String, i64)> = db
        .prepare_cached("SELECT root_session_id, created_at FROM sessions WHERE id = ?1")?
        .query_row([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((root, created_at)) = root else {
        return Ok(None);
    };
    let rows = query_all(
        db,
        "SELECT id, title, parent_session_id, updated_at, head_seq FROM sessions \
         WHERE root_session_id = ?1 ORDER BY created_at, id",
        [&root],
        tree_row,
    )?;
    let (from_ts, to_ts) = events_span(db, &rows)?;
    // A tree without a single event spans the moment its root was created.
    let from_ts = from_ts.unwrap_or(created_at);
    Ok(Some(Tree {
        root,
        sessions: rows.into_iter().map(|(session, ..)| session).collect(),
        from_ts,
        to_ts: to_ts.unwrap_or(from_ts).max(from_ts),
    }))
}

fn tree_row(row: &Row<'_>) -> rusqlite::Result<TreeRow> {
    let session = TreeSession {
        id: row.get(0)?,
        title: row.get(1)?,
        parent_session_id: row.get(2)?,
    };
    Ok((session, row.get(3)?, uint(row, 4)?))
}

/// From the first event of any of the sessions to the last one; `None`
/// when they have no events.
fn events_span(db: &Connection, rows: &[TreeRow]) -> Result<(Option<i64>, Option<i64>)> {
    let (mut from_ts, mut to_ts) = (None::<i64>, None::<i64>);
    for (session, updated_at, head_seq) in rows {
        if *head_seq == 0 {
            continue;
        }
        let first: Option<i64> = query_optional(
            db,
            "SELECT ts FROM events WHERE session_id = ?1 AND seq = 1",
            [&session.id],
        )?;
        if let Some(first) = first {
            from_ts = Some(from_ts.map_or(first, |ts| ts.min(first)));
        }
        to_ts = Some(to_ts.map_or(*updated_at, |ts| ts.max(*updated_at)));
    }
    Ok((from_ts, to_ts))
}

/// The rows `sql` finds for each session of the tree, one session after the
/// other. `?1` is the session and `?2` is `until`.
fn per_session<T>(
    db: &Connection,
    tree: &Tree,
    sql: &str,
    until: i64,
    from_row: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut all = Vec::new();
    for session in &tree.sessions {
        all.extend(query_all(db, sql, params![session.id, until], &from_row)?);
    }
    Ok(all)
}

/// The agents of the tree spawned by `until`, as the index has them now.
fn agents(db: &Connection, tree: &Tree, until: i64) -> Result<Vec<TreeAgent>> {
    per_session(
        db,
        tree,
        "SELECT a.id, a.session_id, a.parent_agent_id, a.kind, a.role, a.runner, a.model, \
         a.status, a.cost_usd, a.tokens_in, a.tokens_out, a.native_session_id, a.spawned_ts, \
         a.label, e.id FROM agents a \
         LEFT JOIN events e ON e.session_id = a.session_id AND e.seq = a.spawned_seq \
         WHERE a.session_id = ?1 AND a.spawned_ts <= ?2 ORDER BY a.spawned_seq, a.id",
        until,
        tree_agent_from_row,
    )
}

fn tree_agent_from_row(row: &Row<'_>) -> rusqlite::Result<TreeAgent> {
    Ok(TreeAgent {
        agent: agent_from_row(row)?,
        spawned_ts: row.get(12)?,
        label: row.get(13)?,
        spawned_event: row.get::<_, Option<String>>(14)?.unwrap_or_default(),
    })
}

/// One `agent_states` row: what changed about an agent at that moment.
struct StateChange {
    agent_id: String,
    status: Option<AgentStatus>,
    totals: Option<(f64, u64, u64)>,
}

fn state_change_from_row(row: &Row<'_>) -> rusqlite::Result<StateChange> {
    let totals = match row.get::<_, Option<f64>>(2)? {
        Some(cost_usd) => Some((cost_usd, uint(row, 3)?, uint(row, 4)?)),
        None => None,
    };
    Ok(StateChange {
        agent_id: row.get(0)?,
        status: from_optional_wire_name(row, 1)?,
        totals,
    })
}

/// Sets each agent's status and totals to what they were at `until`.
fn rewind_agents(db: &Connection, tree: &Tree, until: i64, agents: &mut [TreeAgent]) -> Result<()> {
    let by_id: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(index, agent)| (agent.agent.id.clone(), index))
        .collect();
    let changes = per_session(
        db,
        tree,
        "SELECT agent_id, status, cost_usd, tokens_in, tokens_out FROM agent_states \
         WHERE session_id = ?1 AND ts <= ?2 ORDER BY seq",
        until,
        state_change_from_row,
    )?;
    // Totals only ever come from events: an agent that reported none has none.
    for agent in agents.iter_mut() {
        agent.agent.cost_usd = 0.0;
        agent.agent.tokens_in = 0;
        agent.agent.tokens_out = 0;
    }
    for change in changes {
        let Some(index) = by_id.get(&change.agent_id) else {
            continue;
        };
        let agent = &mut agents[*index].agent;
        if let Some(status) = change.status {
            agent.status = status;
        }
        if let Some((cost_usd, tokens_in, tokens_out)) = change.totals {
            agent.cost_usd = cost_usd;
            agent.tokens_in = tokens_in;
            agent.tokens_out = tokens_out;
        }
    }
    Ok(())
}

/// The messages the tree's agents sent by `until`, per session in log order.
fn sent_messages(db: &Connection, tree: &Tree, until: i64) -> Result<Vec<Sent>> {
    per_session(
        db,
        tree,
        "SELECT id, kind, from_agent_id, to_session_id, to_agent_id, label, ts, event_id \
         FROM messages WHERE session_id = ?1 AND sent = 1 AND ts <= ?2 ORDER BY seq",
        until,
        sent_from_row,
    )
}

fn sent_from_row(row: &Row<'_>) -> rusqlite::Result<Sent> {
    Ok(Sent {
        id: row.get(0)?,
        kind: from_wire_name(row, 1)?,
        from_agent_id: row.get(2)?,
        to_session_id: row.get(3)?,
        to_agent_id: row.get(4)?,
        label: row.get(5)?,
        ts: row.get(6)?,
        event_id: row.get(7)?,
    })
}

/// The agents in spawn-tree order (a parent before its children, siblings by
/// start time), each with the agent it hangs under: `None` is the user.
fn spawn_order(tree: &Tree, agents: Vec<TreeAgent>, messages: &[Sent]) -> Vec<Ordered> {
    let parents = parents(tree, &agents, messages);
    let order = depth_first(&agents, &parents);
    let mut by_id: HashMap<String, TreeAgent> = agents
        .into_iter()
        .map(|agent| (agent.agent.id.clone(), agent))
        .collect();
    order
        .into_iter()
        .filter_map(|id| {
            let parent = parents.get(&id).cloned().flatten();
            by_id.remove(&id).map(|agent| (agent, parent))
        })
        .collect()
}

/// The agent each agent hangs under; `None` is the user. A session's lead
/// hangs under the agent that dispatched the session, or else the lead of
/// the parent session; any other agent under its parent, or else its lead.
fn parents(
    tree: &Tree,
    agents: &[TreeAgent],
    messages: &[Sent],
) -> HashMap<String, Option<String>> {
    let known: HashSet<&str> = agents.iter().map(|agent| agent.agent.id.as_str()).collect();
    let leads = leads(agents.iter().map(|agent| &agent.agent));
    let dispatchers = dispatchers(messages);
    let parent_session: HashMap<&str, Option<&str>> = tree
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session.parent_session_id.as_deref()))
        .collect();

    let parent_of = |agent: &Agent| -> Option<String> {
        let lead = leads.get(agent.session_id.as_str()).copied();
        let candidates: [Option<&str>; 3] = if lead == Some(agent.id.as_str()) {
            let parent = parent_session
                .get(agent.session_id.as_str())
                .copied()
                .flatten();
            [
                dispatchers.get(agent.session_id.as_str()).copied(),
                parent.and_then(|parent| leads.get(parent).copied()),
                None,
            ]
        } else {
            [agent.parent_agent_id.as_deref(), lead, None]
        };
        candidates
            .into_iter()
            .flatten()
            .find(|id| *id != agent.id && known.contains(id))
            .map(str::to_owned)
    };
    agents
        .iter()
        .map(|agent| (agent.agent.id.clone(), parent_of(&agent.agent)))
        .collect()
}

/// A session's lead is its first agent, in the order given, with a process
/// of its own.
fn leads<'a>(agents: impl IntoIterator<Item = &'a Agent>) -> HashMap<&'a str, &'a str> {
    let mut leads = HashMap::new();
    for agent in agents {
        if agent.kind == AgentKind::Process {
            leads
                .entry(agent.session_id.as_str())
                .or_insert(agent.id.as_str());
        }
    }
    leads
}

/// The agent that dispatched each session: the sender of its first dispatch.
fn dispatchers(messages: &[Sent]) -> HashMap<&str, &str> {
    let mut dispatchers = HashMap::new();
    for message in messages {
        if let (MessageKind::Dispatch, Some(from)) = (message.kind, &message.from_agent_id) {
            dispatchers
                .entry(message.to_session_id.as_str())
                .or_insert(from.as_str());
        }
    }
    dispatchers
}

/// The ids of the agents, each parent before its children. Siblings by
/// start time; within a millisecond, in the order of their spawn events.
fn depth_first(agents: &[TreeAgent], parents: &HashMap<String, Option<String>>) -> Vec<String> {
    let mut children: BTreeMap<Option<String>, Vec<(i64, &str, String)>> = BTreeMap::new();
    for agent in agents {
        let parent = parents.get(&agent.agent.id).cloned().flatten();
        let key = (
            agent.spawned_ts,
            agent.spawned_event.as_str(),
            agent.agent.id.clone(),
        );
        children.entry(parent).or_default().push(key);
    }
    for siblings in children.values_mut() {
        siblings.sort();
    }
    let mut order: Vec<String> = Vec::with_capacity(agents.len());
    let mut stack: Vec<String> = children
        .get(&None)
        .map(|roots| roots.iter().rev().map(|key| key.2.clone()).collect())
        .unwrap_or_default();
    while let Some(id) = stack.pop() {
        if let Some(below) = children.get(&Some(id.clone())) {
            stack.extend(below.iter().rev().map(|key| key.2.clone()));
        }
        order.push(id);
    }
    order
}

/// The edges of a graph being put together: one per kind and direction
/// between two nodes, counting what it stands for.
#[derive(Default)]
struct Edges {
    by_id: BTreeMap<String, GraphEdge>,
}

impl Edges {
    fn add(&mut self, kind: EdgeKind, from: &str, to: &str, ts: i64, label: Option<&str>) {
        // Sorted by id, which makes the order of the result stable too.
        let id = format!("{}:{from}:{to}", edge_name(kind));
        let edge = self.by_id.entry(id.clone()).or_insert_with(|| GraphEdge {
            id,
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
            count: 0,
            last_ts: ts,
            label: None,
        });
        edge.count += 1;
        if ts >= edge.last_ts {
            edge.last_ts = ts;
            edge.label = label.filter(|label| !label.is_empty()).map(str::to_owned);
        }
    }
}

fn edge_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Spawned => "spawned",
        EdgeKind::Dispatched => "dispatched",
        EdgeKind::Message => "message",
        EdgeKind::Report => "report",
        EdgeKind::Changed => "changed",
    }
}

fn message_edge(kind: MessageKind) -> EdgeKind {
    match kind {
        MessageKind::Dispatch => EdgeKind::Dispatched,
        MessageKind::Note => EdgeKind::Message,
        MessageKind::Report => EdgeKind::Report,
    }
}

/// `live` edits the user's own checkout, `write` a private worktree, `read` nothing.
fn strength(mode: RepoMode) -> u8 {
    match mode {
        RepoMode::Read => 0,
        RepoMode::Write => 1,
        RepoMode::Live => 2,
    }
}

/// The graph of the tree `session_id` belongs to, as of `at` (clamped to
/// the tree's time range) or as of now. `None` for an unknown session.
pub(super) fn graph(db: &Connection, session_id: &str, at: Option<i64>) -> Result<Option<Graph>> {
    let Some(tree) = tree(db, session_id)? else {
        return Ok(None);
    };
    let at = at.map_or(tree.to_ts, |at| at.clamp(tree.from_ts, tree.to_ts));

    let mut agents = agents(db, &tree, at)?;
    rewind_agents(db, &tree, at, &mut agents)?;
    let messages = sent_messages(db, &tree, at)?;
    let ordered = spawn_order(&tree, agents, &messages);
    let statuses = session_statuses(db, &tree, at)?;

    let mut edges = Edges::default();
    let mut nodes = vec![user_node()];
    nodes.extend(agent_nodes(&tree, &ordered, &statuses, &mut edges));
    nodes.extend(repo_nodes(db, &tree, at)?);

    // An edge needs both its ends: a message to an agent that had not
    // started by `at` shows once that agent does.
    let present: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    message_edges(&messages, &present, &mut edges);
    change_edges(db, &tree, at, &present, &mut edges)?;

    Ok(Some(Graph {
        root_session_id: tree.root,
        nodes,
        edges: edges.by_id.into_values().collect(),
        from_ts: tree.from_ts,
        to_ts: tree.to_ts,
        at,
    }))
}

/// Each session's status at `at`: `queued` before it had one.
fn session_statuses<'a>(
    db: &Connection,
    tree: &'a Tree,
    at: i64,
) -> Result<HashMap<&'a str, SessionStatus>> {
    let mut statuses = HashMap::new();
    for session in &tree.sessions {
        let status = db
            .prepare_cached(
                "SELECT status FROM session_states WHERE session_id = ?1 AND ts <= ?2 \
                 ORDER BY seq DESC LIMIT 1",
            )?
            .query_row(params![session.id, at], |row| from_wire_name(row, 0))
            .optional()?;
        statuses.insert(session.id.as_str(), status.unwrap_or(SessionStatus::Queued));
    }
    Ok(statuses)
}

fn user_node() -> GraphNode {
    GraphNode {
        id: USER_NODE.to_owned(),
        kind: NodeKind::User,
        label: USER_LABEL.to_owned(),
        parent_id: None,
        agent: None,
        session_id: None,
        session_status: None,
        session_title: None,
        repo_id: None,
        repo_mode: None,
    }
}

/// One node per agent, in spawn-tree order, and the edge of each subagent's spawn.
fn agent_nodes(
    tree: &Tree,
    ordered: &[Ordered],
    statuses: &HashMap<&str, SessionStatus>,
    edges: &mut Edges,
) -> Vec<GraphNode> {
    let titles: HashMap<&str, &str> = tree
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session.title.as_str()))
        .collect();
    let leads = leads(ordered.iter().map(|(agent, _)| &agent.agent));
    let mut nodes = Vec::with_capacity(ordered.len());
    for (agent, parent) in ordered {
        let session = agent.agent.session_id.as_str();
        let lead = leads.get(session) == Some(&agent.agent.id.as_str());
        let title = titles.get(session).copied();
        let node = agent_node(&agent.agent.id);
        if let (AgentKind::Subagent, Some(parent)) = (agent.agent.kind, parent) {
            let label = agent.label.as_deref().unwrap_or(&agent.agent.role);
            edges.add(
                EdgeKind::Spawned,
                &agent_node(parent),
                &node,
                agent.spawned_ts,
                Some(label),
            );
        }
        nodes.push(GraphNode {
            id: node,
            kind: NodeKind::Agent,
            label: agent_label(agent, lead, title),
            parent_id: Some(participant(parent.as_deref())),
            agent: Some(agent.agent.clone()),
            session_id: Some(session.to_owned()),
            session_status: lead.then(|| statuses.get(session).copied()).flatten(),
            session_title: lead.then(|| title.map(str::to_owned)).flatten(),
            repo_id: None,
            repo_mode: None,
        });
    }
    nodes
}

/// A lead is named after its session, any other agent after what it was
/// asked to do, and either one after its role when there is no such text.
fn agent_label(agent: &TreeAgent, lead: bool, title: Option<&str>) -> String {
    match (&agent.label, lead, title) {
        (_, true, Some(title)) if !title.is_empty() => title.to_owned(),
        (Some(label), _, _) if !label.is_empty() => label.clone(),
        _ => agent.agent.role.clone(),
    }
}

/// One node per registry repo, however many sessions attach it, with the
/// strongest mode any of them has; by name.
fn repo_nodes(db: &Connection, tree: &Tree, at: i64) -> Result<Vec<GraphNode>> {
    let attached = per_session(
        db,
        tree,
        "SELECT repo_id, name, mode FROM session_repos \
         WHERE session_id = ?1 AND attached_ts <= ?2 ORDER BY attached_seq, id",
        at,
        attachment_from_row,
    )?;
    let mut repos: BTreeMap<String, (String, RepoMode)> = BTreeMap::new();
    for (repo_id, name, mode) in attached {
        let known = repos.entry(repo_id).or_insert((name, mode));
        if strength(mode) > strength(known.1) {
            known.1 = mode;
        }
    }
    let mut nodes: Vec<GraphNode> = repos
        .into_iter()
        .map(|(repo_id, (name, mode))| GraphNode {
            id: repo_node(&repo_id),
            kind: NodeKind::Repo,
            label: name,
            parent_id: None,
            agent: None,
            session_id: None,
            session_status: None,
            session_title: None,
            repo_id: Some(repo_id),
            repo_mode: Some(mode),
        })
        .collect();
    nodes.sort_by(|a, b| (&a.label, &a.id).cmp(&(&b.label, &b.id)));
    Ok(nodes)
}

/// A repo's id, name and mode as one session attached it.
fn attachment_from_row(row: &Row<'_>) -> rusqlite::Result<(String, String, RepoMode)> {
    Ok((row.get(0)?, row.get(1)?, from_wire_name(row, 2)?))
}

fn message_edges(messages: &[Sent], present: &HashSet<&str>, edges: &mut Edges) {
    for message in messages {
        let from = participant(message.from_agent_id.as_deref());
        let to = participant(message.to_agent_id.as_deref());
        if present.contains(from.as_str()) && present.contains(to.as_str()) {
            let label = Some(message.label.as_str());
            edges.add(message_edge(message.kind), &from, &to, message.ts, label);
        }
    }
}

/// A file change: the agent that made it, the repo, the path and when.
type Change = (Option<String>, String, String, i64);

fn change_edges(
    db: &Connection,
    tree: &Tree,
    at: i64,
    present: &HashSet<&str>,
    edges: &mut Edges,
) -> Result<()> {
    let changes = per_session(
        db,
        tree,
        "SELECT f.agent_id, r.repo_id, f.path, f.ts FROM file_changes f \
         JOIN session_repos r ON r.id = f.session_repo_id AND r.session_id = f.session_id \
         WHERE f.session_id = ?1 AND f.ts <= ?2 ORDER BY f.seq",
        at,
        change_from_row,
    )?;
    for (agent_id, repo_id, path, ts) in changes {
        let (from, to) = (participant(agent_id.as_deref()), repo_node(&repo_id));
        if present.contains(from.as_str()) && present.contains(to.as_str()) {
            edges.add(EdgeKind::Changed, &from, &to, ts, Some(&path));
        }
    }
    Ok(())
}

fn change_from_row(row: &Row<'_>) -> rusqlite::Result<Change> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

/// The timeline of the tree `session_id` belongs to. `None` for an unknown session.
pub(super) fn timeline(db: &Connection, session_id: &str) -> Result<Option<Timeline>> {
    let Some(tree) = tree(db, session_id)? else {
        return Ok(None);
    };
    let agents = agents(db, &tree, i64::MAX)?;
    let messages = sent_messages(db, &tree, i64::MAX)?;
    let ordered = spawn_order(&tree, agents, &messages);
    let mut spans = spans_by_agent(db, &tree)?;
    let arrows = arrows(&ordered, &messages);

    let lanes = ordered
        .into_iter()
        .map(|(agent, parent)| {
            let mut spans = spans.remove(&agent.agent.id).unwrap_or_default();
            // Stable: spans that start in the same millisecond keep log order.
            spans.sort_by_key(|span| span.start);
            TimelineLane {
                session_id: agent.agent.session_id.clone(),
                parent_id: Some(participant(parent.as_deref())),
                agent: agent.agent,
                spans,
            }
        })
        .collect();

    Ok(Some(Timeline {
        root_session_id: tree.root,
        lanes,
        arrows,
        from_ts: tree.from_ts,
        to_ts: tree.to_ts,
    }))
}

/// The spans of the tree's sessions by agent, in log order.
fn spans_by_agent(db: &Connection, tree: &Tree) -> Result<HashMap<String, Vec<TimelineSpan>>> {
    let mut spans: HashMap<String, Vec<TimelineSpan>> = HashMap::new();
    for session in &tree.sessions {
        let rows = query_all(
            db,
            "SELECT id, agent_id, kind, title, tool_kind, status, start_ts, end_ts FROM spans \
             WHERE session_id = ?1 ORDER BY seq",
            [&session.id],
            span_from_row,
        )?;
        for (agent_id, span) in rows {
            spans.entry(agent_id).or_default().push(span);
        }
    }
    Ok(spans)
}

/// A span and the agent whose lane it is in.
fn span_from_row(row: &Row<'_>) -> rusqlite::Result<(String, TimelineSpan)> {
    let agent_id = row.get(1)?;
    let span = TimelineSpan {
        id: row.get(0)?,
        kind: from_wire_name(row, 2)?,
        title: row.get(3)?,
        tool_kind: from_optional_wire_name(row, 4)?,
        status: from_optional_wire_name(row, 5)?,
        start: row.get(6)?,
        end: row.get(7)?,
    };
    Ok((agent_id, span))
}

/// The arrows between lanes, in the order they happened.
fn arrows(ordered: &[Ordered], messages: &[Sent]) -> Vec<TimelineArrow> {
    // Each arrow with the event it stands for, which orders the arrows of
    // one millisecond the way they happened.
    let mut arrows = message_arrows(ordered, messages);
    arrows.extend(spawn_arrows(ordered));
    arrows.sort_by(|a, b| (a.1.ts, a.0, &a.1.id).cmp(&(b.1.ts, b.0, &b.1.id)));
    arrows.into_iter().map(|(_, arrow)| arrow).collect()
}

fn message_arrows<'a>(ordered: &[Ordered], messages: &'a [Sent]) -> Vec<(&'a str, TimelineArrow)> {
    let lanes_of: HashSet<&str> = ordered
        .iter()
        .map(|(agent, _)| agent.agent.id.as_str())
        .collect();
    // An arrow joins two lanes, or a lane and the user: one that names an
    // agent without a lane (a child that has not started) shows once it has one.
    let has_lane = |agent_id: &Option<String>| {
        agent_id
            .as_ref()
            .is_none_or(|id| lanes_of.contains(id.as_str()))
    };
    messages
        .iter()
        .filter(|message| has_lane(&message.from_agent_id) && has_lane(&message.to_agent_id))
        .map(|message| {
            let arrow = TimelineArrow {
                id: format!("message:{}", message.id),
                kind: message_edge(message.kind),
                from_agent_id: message.from_agent_id.clone(),
                to_agent_id: message.to_agent_id.clone(),
                ts: message.ts,
                label: Some(message.label.clone()).filter(|label| !label.is_empty()),
            };
            (message.event_id.as_str(), arrow)
        })
        .collect()
}

fn spawn_arrows(ordered: &[Ordered]) -> Vec<(&str, TimelineArrow)> {
    ordered
        .iter()
        .filter_map(|(agent, parent)| {
            let (AgentKind::Subagent, Some(parent)) = (agent.agent.kind, parent) else {
                return None;
            };
            let arrow = TimelineArrow {
                id: format!("spawned:{}", agent.agent.id),
                kind: EdgeKind::Spawned,
                from_agent_id: Some(parent.clone()),
                to_agent_id: Some(agent.agent.id.clone()),
                ts: agent.spawned_ts,
                label: Some(
                    agent
                        .label
                        .clone()
                        .unwrap_or_else(|| agent.agent.role.clone()),
                ),
            };
            Some((agent.spawned_event.as_str(), arrow))
        })
        .collect()
}
