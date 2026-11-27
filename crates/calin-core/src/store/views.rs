//! The graph and the timeline of a session tree, read from the index alone
//! (the semantics in `calin-proto/src/graph.rs`). Nothing here reads a log:
//! every row was projected when its event was indexed, so a request costs a
//! few small queries per session of the tree, however long the logs are.

use std::collections::{BTreeMap, HashMap, HashSet};

use calin_proto::{
    Agent, AgentKind, EdgeKind, Graph, GraphEdge, GraphNode, MessageKind, NodeKind, RepoMode,
    SessionStatus, Timeline, TimelineArrow, TimelineLane, TimelineSpan,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::index::{from_wire_name, uint};
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

fn tree(db: &Connection, session_id: &str) -> Result<Option<Tree>> {
    let root: Option<(String, i64)> = db
        .prepare_cached("SELECT root_session_id, created_at FROM sessions WHERE id = ?1")?
        .query_row([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((root, created_at)) = root else {
        return Ok(None);
    };
    let mut statement = db.prepare_cached(
        "SELECT id, title, parent_session_id, updated_at, head_seq FROM sessions \
         WHERE root_session_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([&root], |row| {
        let session = TreeSession {
            id: row.get(0)?,
            title: row.get(1)?,
            parent_session_id: row.get(2)?,
        };
        Ok((session, row.get::<_, i64>(3)?, uint(row, 4)?))
    })?;
    let mut sessions = Vec::new();
    let (mut from_ts, mut to_ts) = (None::<i64>, None::<i64>);
    let mut first_event =
        db.prepare_cached("SELECT ts FROM events WHERE session_id = ?1 AND seq = 1")?;
    for row in rows {
        let (session, updated_at, head_seq) = row?;
        if head_seq > 0 {
            let first: Option<i64> = first_event
                .query_row([&session.id], |row| row.get(0))
                .optional()?;
            if let Some(first) = first {
                from_ts = Some(from_ts.map_or(first, |ts| ts.min(first)));
            }
            to_ts = Some(to_ts.map_or(updated_at, |ts| ts.max(updated_at)));
        }
        sessions.push(session);
    }
    // A tree without a single event spans the moment its root was created.
    let from_ts = from_ts.unwrap_or(created_at);
    Ok(Some(Tree {
        root,
        sessions,
        from_ts,
        to_ts: to_ts.unwrap_or(from_ts).max(from_ts),
    }))
}

/// The agents of the tree spawned by `until`, as the index has them now.
fn agents(db: &Connection, tree: &Tree, until: i64) -> Result<Vec<TreeAgent>> {
    let mut statement = db.prepare_cached(
        "SELECT a.id, a.session_id, a.parent_agent_id, a.kind, a.role, a.runner, a.model, \
         a.status, a.cost_usd, a.tokens_in, a.tokens_out, a.native_session_id, a.spawned_ts, \
         a.label, e.id FROM agents a \
         LEFT JOIN events e ON e.session_id = a.session_id AND e.seq = a.spawned_seq \
         WHERE a.session_id = ?1 AND a.spawned_ts <= ?2 ORDER BY a.spawned_seq, a.id",
    )?;
    let mut all = Vec::new();
    for session in &tree.sessions {
        let rows = statement.query_map(params![session.id, until], |row| {
            Ok(TreeAgent {
                agent: Agent {
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
                },
                spawned_ts: row.get(12)?,
                label: row.get(13)?,
                spawned_event: row.get::<_, Option<String>>(14)?.unwrap_or_default(),
            })
        })?;
        for row in rows {
            all.push(row?);
        }
    }
    Ok(all)
}

/// Sets each agent's status and totals to what they were at `until`.
fn rewind_agents(db: &Connection, tree: &Tree, until: i64, agents: &mut [TreeAgent]) -> Result<()> {
    let by_id: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(index, agent)| (agent.agent.id.clone(), index))
        .collect();
    let mut statement = db.prepare_cached(
        "SELECT agent_id, status, cost_usd, tokens_in, tokens_out FROM agent_states \
         WHERE session_id = ?1 AND ts <= ?2 ORDER BY seq",
    )?;
    // Totals only ever come from events: an agent that reported none has none.
    for agent in agents.iter_mut() {
        agent.agent.cost_usd = 0.0;
        agent.agent.tokens_in = 0;
        agent.agent.tokens_out = 0;
    }
    for session in &tree.sessions {
        let mut rows = statement.query(params![session.id, until])?;
        while let Some(row) = rows.next()? {
            let agent_id: String = row.get(0)?;
            let Some(agent) = by_id.get(&agent_id).map(|index| &mut agents[*index].agent) else {
                continue;
            };
            if row.get::<_, Option<String>>(1)?.is_some() {
                agent.status = from_wire_name(row, 1)?;
            }
            if let Some(cost_usd) = row.get::<_, Option<f64>>(2)? {
                agent.cost_usd = cost_usd;
                agent.tokens_in = uint(row, 3)?;
                agent.tokens_out = uint(row, 4)?;
            }
        }
    }
    Ok(())
}

/// The messages the tree's agents sent by `until`, per session in log order.
fn sent_messages(db: &Connection, tree: &Tree, until: i64) -> Result<Vec<Sent>> {
    let mut statement = db.prepare_cached(
        "SELECT id, kind, from_agent_id, to_session_id, to_agent_id, label, ts, event_id \
         FROM messages WHERE session_id = ?1 AND sent = 1 AND ts <= ?2 ORDER BY seq",
    )?;
    let mut all = Vec::new();
    for session in &tree.sessions {
        let rows = statement.query_map(params![session.id, until], |row| {
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
        })?;
        for row in rows {
            all.push(row?);
        }
    }
    Ok(all)
}

/// The agents in spawn-tree order (a parent before its children, siblings by
/// start time), each with the agent it hangs under: `None` is the user.
fn spawn_order(
    tree: &Tree,
    agents: Vec<TreeAgent>,
    messages: &[Sent],
) -> Vec<(TreeAgent, Option<String>)> {
    let known: HashMap<&str, &TreeAgent> = agents
        .iter()
        .map(|agent| (agent.agent.id.as_str(), agent))
        .collect();
    // A session's lead is its first agent with a process of its own.
    let mut leads: HashMap<&str, &str> = HashMap::new();
    for agent in &agents {
        if agent.agent.kind == AgentKind::Process {
            leads
                .entry(agent.agent.session_id.as_str())
                .or_insert(agent.agent.id.as_str());
        }
    }
    let mut dispatchers: HashMap<&str, &str> = HashMap::new();
    for message in messages {
        if let (MessageKind::Dispatch, Some(from)) = (message.kind, &message.from_agent_id) {
            dispatchers
                .entry(message.to_session_id.as_str())
                .or_insert(from.as_str());
        }
    }
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
            .find(|id| *id != agent.id && known.contains_key(id))
            .map(str::to_owned)
    };
    let parents: HashMap<String, Option<String>> = agents
        .iter()
        .map(|agent| (agent.agent.id.clone(), parent_of(&agent.agent)))
        .collect();

    // Siblings by start time; within a millisecond, in the order of their
    // spawn events.
    let mut children: BTreeMap<Option<String>, Vec<(i64, &str, String)>> = BTreeMap::new();
    for agent in &agents {
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

    let mut session_status = db.prepare_cached(
        "SELECT status FROM session_states WHERE session_id = ?1 AND ts <= ?2 \
         ORDER BY seq DESC LIMIT 1",
    )?;
    let mut statuses: HashMap<&str, SessionStatus> = HashMap::new();
    for session in &tree.sessions {
        let status = session_status
            .query_row(params![session.id, at], |row| from_wire_name(row, 0))
            .optional()?;
        statuses.insert(&session.id, status.unwrap_or(SessionStatus::Queued));
    }
    let titles: HashMap<&str, &str> = tree
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session.title.as_str()))
        .collect();

    let mut nodes = vec![GraphNode {
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
    }];
    let mut edges = Edges::default();
    let mut leads: HashMap<&str, &str> = HashMap::new();
    for (agent, _) in &ordered {
        if agent.agent.kind == AgentKind::Process {
            leads
                .entry(agent.agent.session_id.as_str())
                .or_insert(agent.agent.id.as_str());
        }
    }
    for (agent, parent) in &ordered {
        let session = agent.agent.session_id.as_str();
        let lead = leads.get(session) == Some(&agent.agent.id.as_str());
        let title = titles.get(session).copied();
        let label = match (&agent.label, lead, title) {
            (_, true, Some(title)) if !title.is_empty() => title.to_owned(),
            (Some(label), _, _) if !label.is_empty() => label.clone(),
            _ => agent.agent.role.clone(),
        };
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
            label,
            parent_id: Some(participant(parent.as_deref())),
            agent: Some(agent.agent.clone()),
            session_id: Some(session.to_owned()),
            session_status: lead.then(|| statuses.get(session).copied()).flatten(),
            session_title: lead.then(|| title.map(str::to_owned)).flatten(),
            repo_id: None,
            repo_mode: None,
        });
    }

    // One node per registry repo, however many sessions attach it.
    let mut repos: BTreeMap<String, (String, RepoMode)> = BTreeMap::new();
    let mut attached = db.prepare_cached(
        "SELECT repo_id, name, mode FROM session_repos \
         WHERE session_id = ?1 AND attached_ts <= ?2 ORDER BY attached_seq, id",
    )?;
    for session in &tree.sessions {
        let mut rows = attached.query(params![session.id, at])?;
        while let Some(row) = rows.next()? {
            let (repo_id, name): (String, String) = (row.get(0)?, row.get(1)?);
            let mode: RepoMode = from_wire_name(row, 2)?;
            let known = repos.entry(repo_id).or_insert((name, mode));
            if strength(mode) > strength(known.1) {
                known.1 = mode;
            }
        }
    }
    let mut repo_nodes: Vec<GraphNode> = repos
        .iter()
        .map(|(repo_id, (name, mode))| GraphNode {
            id: repo_node(repo_id),
            kind: NodeKind::Repo,
            label: name.clone(),
            parent_id: None,
            agent: None,
            session_id: None,
            session_status: None,
            session_title: None,
            repo_id: Some(repo_id.clone()),
            repo_mode: Some(*mode),
        })
        .collect();
    repo_nodes.sort_by(|a, b| (&a.label, &a.id).cmp(&(&b.label, &b.id)));
    nodes.extend(repo_nodes);

    // An edge needs both its ends: a message to an agent that had not
    // started by `at` shows once that agent does.
    let present: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    for message in &messages {
        let from = participant(message.from_agent_id.as_deref());
        let to = participant(message.to_agent_id.as_deref());
        if present.contains(from.as_str()) && present.contains(to.as_str()) {
            let label = Some(message.label.as_str());
            edges.add(message_edge(message.kind), &from, &to, message.ts, label);
        }
    }

    let mut changes = db.prepare_cached(
        "SELECT f.agent_id, r.repo_id, f.path, f.ts FROM file_changes f \
         JOIN session_repos r ON r.id = f.session_repo_id AND r.session_id = f.session_id \
         WHERE f.session_id = ?1 AND f.ts <= ?2 ORDER BY f.seq",
    )?;
    for session in &tree.sessions {
        let mut rows = changes.query(params![session.id, at])?;
        while let Some(row) = rows.next()? {
            let agent_id: Option<String> = row.get(0)?;
            let (repo_id, path): (String, String) = (row.get(1)?, row.get(2)?);
            let (from, to) = (participant(agent_id.as_deref()), repo_node(&repo_id));
            if present.contains(from.as_str()) && present.contains(to.as_str()) {
                edges.add(EdgeKind::Changed, &from, &to, row.get(3)?, Some(&path));
            }
        }
    }

    Ok(Some(Graph {
        root_session_id: tree.root,
        nodes,
        edges: edges.by_id.into_values().collect(),
        from_ts: tree.from_ts,
        to_ts: tree.to_ts,
        at,
    }))
}

/// The timeline of the tree `session_id` belongs to. `None` for an unknown session.
pub(super) fn timeline(db: &Connection, session_id: &str) -> Result<Option<Timeline>> {
    let Some(tree) = tree(db, session_id)? else {
        return Ok(None);
    };
    let agents = agents(db, &tree, i64::MAX)?;
    let messages = sent_messages(db, &tree, i64::MAX)?;
    let ordered = spawn_order(&tree, agents, &messages);

    let mut spans: HashMap<String, Vec<TimelineSpan>> = HashMap::new();
    let mut statement = db.prepare_cached(
        "SELECT id, agent_id, kind, title, tool_kind, status, start_ts, end_ts FROM spans \
         WHERE session_id = ?1 ORDER BY seq",
    )?;
    for session in &tree.sessions {
        let mut rows = statement.query([&session.id])?;
        while let Some(row) = rows.next()? {
            let agent_id: String = row.get(1)?;
            let tool_kind = match row.get::<_, Option<String>>(4)? {
                Some(_) => Some(from_wire_name(row, 4)?),
                None => None,
            };
            let status = match row.get::<_, Option<String>>(5)? {
                Some(_) => Some(from_wire_name(row, 5)?),
                None => None,
            };
            spans.entry(agent_id).or_default().push(TimelineSpan {
                id: row.get(0)?,
                kind: from_wire_name(row, 2)?,
                title: row.get(3)?,
                tool_kind,
                status,
                start: row.get(6)?,
                end: row.get(7)?,
            });
        }
    }

    // Each arrow with the event it stands for, which orders the arrows of
    // one millisecond the way they happened.
    let mut arrows: Vec<(&str, TimelineArrow)> = Vec::new();
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
    for message in &messages {
        if has_lane(&message.from_agent_id) && has_lane(&message.to_agent_id) {
            let arrow = TimelineArrow {
                id: format!("message:{}", message.id),
                kind: message_edge(message.kind),
                from_agent_id: message.from_agent_id.clone(),
                to_agent_id: message.to_agent_id.clone(),
                ts: message.ts,
                label: Some(message.label.clone()).filter(|label| !label.is_empty()),
            };
            arrows.push((&message.event_id, arrow));
        }
    }
    for (agent, parent) in &ordered {
        if let (AgentKind::Subagent, Some(parent)) = (agent.agent.kind, parent) {
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
            arrows.push((&agent.spawned_event, arrow));
        }
    }
    arrows.sort_by(|a, b| (a.1.ts, a.0, &a.1.id).cmp(&(b.1.ts, b.0, &b.1.id)));
    let arrows: Vec<TimelineArrow> = arrows.into_iter().map(|(_, arrow)| arrow).collect();

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
