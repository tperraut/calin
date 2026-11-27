// Builders for the orchestration views' tests: agents, messages, a graph and a timeline of one small tree.
import type { Agent } from "../gen/Agent";
import type { AgentMessage } from "../gen/AgentMessage";
import type { EdgeKind } from "../gen/EdgeKind";
import type { Graph } from "../gen/Graph";
import type { GraphEdge } from "../gen/GraphEdge";
import type { GraphNode } from "../gen/GraphNode";
import type { Timeline } from "../gen/Timeline";
import type { TimelineSpan } from "../gen/TimelineSpan";

export function makeAgent(id: string, patch: Partial<Agent> = {}): Agent {
  return {
    id,
    session_id: "s1",
    parent_agent_id: null,
    kind: "process",
    role: "lead",
    runner: "mock",
    model: null,
    native_session_id: null,
    status: "running",
    cost_usd: 0,
    tokens_in: 0,
    tokens_out: 0,
    ...patch,
  };
}

export const makeSubagent = (id: string, role: string, patch: Partial<Agent> = {}): Agent =>
  makeAgent(id, { kind: "subagent", parent_agent_id: "a1", role, ...patch });

export function makeMessage(id: string, patch: Partial<AgentMessage> = {}): AgentMessage {
  return {
    id,
    kind: "note",
    from_session_id: "s1",
    from_agent_id: "a1",
    to_session_id: "s2",
    to_agent_id: "a2",
    body: "Hello",
    in_reply_to: null,
    ts: 1000,
    ...patch,
  };
}

const blank = {
  parent_id: null,
  agent: null,
  session_id: null,
  session_status: null,
  session_title: null,
  repo_id: null,
  repo_mode: null,
} as const;

export const userNode = (): GraphNode => ({ ...blank, id: "user", kind: "user", label: "You" });

export function agentNode(
  agent: Agent,
  parentId: string,
  patch: Partial<GraphNode> = {},
): GraphNode {
  return {
    ...blank,
    id: `agent:${agent.id}`,
    kind: "agent",
    label: agent.role,
    parent_id: parentId,
    agent,
    session_id: agent.session_id,
    ...patch,
  };
}

export const repoNode = (id: string, patch: Partial<GraphNode> = {}): GraphNode => ({
  ...blank,
  id: `repo:${id}`,
  kind: "repo",
  label: id,
  repo_id: id,
  repo_mode: "write",
  ...patch,
});

export function makeEdge(
  kind: EdgeKind,
  from: string,
  to: string,
  patch: Partial<GraphEdge> = {},
): GraphEdge {
  return {
    id: `${kind}:${from}:${to}`,
    from,
    to,
    kind,
    count: 1,
    last_ts: 5000,
    label: null,
    ...patch,
  };
}

export const T0 = 1_800_000_000_000;

/**
 * A lead (`a1`, session `s1`) with two subagents, two dispatched children
 * (`a2` in `s2`, `a3` in `s3`, which has a subagent of its own) and three
 * repos, with one edge of every kind.
 */
export function makeGraph(patch: Partial<Graph> = {}): Graph {
  const lead = makeAgent("a1", { cost_usd: 0.0421, tokens_in: 3800, tokens_out: 460 });
  const worker = makeAgent("a2", { session_id: "s2", status: "idle", model: "mock-1" });
  const badge = makeAgent("a3", { session_id: "s3" });
  return {
    root_session_id: "s1",
    nodes: [
      userNode(),
      agentNode(lead, "user", {
        session_status: "running",
        session_title: "Ship the health check",
      }),
      agentNode(makeSubagent("sub1", "explorer", { status: "done" }), "agent:a1"),
      agentNode(makeSubagent("sub2", "reviewer"), "agent:a1"),
      agentNode(worker, "agent:a1", {
        label: "worker",
        session_status: "review",
        session_title: "API: add /health",
      }),
      agentNode(badge, "agent:a1", {
        label: "worker",
        session_status: "running",
        session_title: "Web: status badge",
      }),
      agentNode(
        makeSubagent("sub3", "tester", { session_id: "s3", parent_agent_id: "a3" }),
        "agent:a3",
      ),
      repoNode("api"),
      repoNode("web"),
      repoNode("docs", { repo_mode: "read" }),
    ],
    edges: [
      makeEdge("spawned", "agent:a1", "agent:sub1"),
      makeEdge("spawned", "agent:a1", "agent:sub2"),
      makeEdge("dispatched", "agent:a1", "agent:a2", { label: "Add GET /health to the API." }),
      makeEdge("dispatched", "agent:a1", "agent:a3"),
      makeEdge("spawned", "agent:a3", "agent:sub3"),
      makeEdge("message", "agent:a2", "agent:a1", { count: 2, label: "Done, tests next." }),
      makeEdge("message", "agent:a1", "agent:a2", { label: "Add /health next to it." }),
      makeEdge("report", "agent:a2", "agent:a1", { label: "GET /health returns the version." }),
      makeEdge("changed", "agent:a2", "repo:api", { count: 4, label: "tests/health.rs" }),
      makeEdge("changed", "agent:a1", "repo:api", { label: "README.md" }),
      makeEdge("changed", "agent:sub3", "repo:web", { label: "src/Badge.tsx" }),
    ],
    from_ts: T0,
    to_ts: T0 + 60_000,
    at: T0 + 60_000,
    ...patch,
  };
}

export function makeSpan(id: string, patch: Partial<TimelineSpan> = {}): TimelineSpan {
  return {
    id,
    kind: "turn",
    title: id,
    tool_kind: null,
    status: null,
    start: T0,
    end: T0 + 10_000,
    ...patch,
  };
}

/** The lead with a tool call and a wait, a subagent, and a child whose turn is still open. */
export function makeTimeline(patch: Partial<Timeline> = {}): Timeline {
  return {
    root_session_id: "s1",
    lanes: [
      {
        agent: makeAgent("a1"),
        session_id: "s1",
        parent_id: "user",
        spans: [
          makeSpan("turn-1", { title: "Ship the health check", end: T0 + 40_000 }),
          makeSpan("t1", {
            kind: "tool",
            title: "cargo test --workspace",
            tool_kind: "execute",
            status: "completed",
            start: T0 + 5000,
            end: T0 + 17_000,
          }),
          makeSpan("wait-1", {
            kind: "waiting",
            title: "Question: header or footer?",
            start: T0 + 20_000,
            end: T0 + 30_000,
          }),
        ],
      },
      {
        agent: makeSubagent("sub1", "explorer", { status: "done" }),
        session_id: "s1",
        parent_id: "agent:a1",
        spans: [makeSpan("turn-sub1", { title: "Explore", start: T0 + 2000, end: T0 + 8000 })],
      },
      {
        agent: makeAgent("a2", { session_id: "s2" }),
        session_id: "s2",
        parent_id: "agent:a1",
        spans: [
          makeSpan("turn-a2", { title: "API: add /health", start: T0 + 10_000, end: null }),
          makeSpan("c1", {
            kind: "tool",
            title: "src/health.rs",
            tool_kind: "edit",
            status: "running",
            start: T0 + 50_000,
            end: null,
          }),
        ],
      },
    ],
    arrows: [
      {
        id: "ar1",
        kind: "message",
        from_agent_id: null,
        to_agent_id: "a1",
        ts: T0,
        label: "Ship it",
      },
      {
        id: "ar2",
        kind: "spawned",
        from_agent_id: "a1",
        to_agent_id: "sub1",
        ts: T0 + 2000,
        label: "explorer",
      },
      {
        id: "ar3",
        kind: "dispatched",
        from_agent_id: "a1",
        to_agent_id: "a2",
        ts: T0 + 10_000,
        label: "API: add /health",
      },
      {
        id: "ar4",
        kind: "report",
        from_agent_id: "a2",
        to_agent_id: "a1",
        ts: T0 + 55_000,
        label: "Done",
      },
    ],
    from_ts: T0,
    to_ts: T0 + 60_000,
    ...patch,
  };
}
