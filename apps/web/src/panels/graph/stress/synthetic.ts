import type { Agent } from "../../../gen/Agent";
import type { EdgeKind } from "../../../gen/EdgeKind";
import type { Graph } from "../../../gen/Graph";
import type { GraphEdge } from "../../../gen/GraphEdge";
import type { GraphNode } from "../../../gen/GraphNode";

/** A small seeded generator, so two runs measure the same graph. */
function mulberry32(seed: number): () => number {
  let state = seed;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** One arrival: a node, with the edge that hangs it in the tree when it has one. */
export type Step = { node: GraphNode; edges: GraphEdge[] };

const T0 = 1_800_000_000_000;

function agent(
  id: string,
  sessionId: string,
  parent: string | null,
  subagent: boolean,
  role: string,
  running: boolean,
): Agent {
  return {
    id,
    session_id: sessionId,
    parent_agent_id: parent,
    kind: subagent ? "subagent" : "process",
    role,
    runner: "mock",
    model: null,
    native_session_id: null,
    status: running ? "running" : "done",
    cost_usd: subagent ? 0 : 0.12,
    tokens_in: 1200,
    tokens_out: 300,
  };
}

/**
 * A synthetic session tree in arrival order: the user, a lead, dispatched
 * children, in-process subagents under leads and children, and repos; then
 * `edgeCount - (nodes in the tree)` message, report and changed edges spread
 * over the arrivals. The shape of the spike's graph (spikes/graph-layout).
 */
export function synthesize(nodeCount: number, edgeCount: number, seed = 7): Step[] {
  const random = mulberry32(seed);
  const pick = <T>(list: readonly T[]): T => list[Math.floor(random() * list.length)];
  const steps: Step[] = [];
  const edge = (kind: EdgeKind, from: string, to: string, at: number, count = 1): GraphEdge => ({
    id: `${kind}:${from}:${to}`,
    from,
    to,
    kind,
    count,
    last_ts: T0 + at * 1000,
    label: kind === "changed" ? "src/lib.rs" : kind === "message" ? "On it." : null,
  });
  const repoCount = Math.max(3, Math.min(8, Math.round(nodeCount / 25)));
  steps.push({
    node: {
      id: "user",
      kind: "user",
      label: "You",
      parent_id: null,
      agent: null,
      session_id: null,
      session_status: null,
      session_title: null,
      repo_id: null,
      repo_mode: null,
    },
    edges: [],
  });
  const leads: string[] = [];
  const agents: string[] = [];
  const sessionOf = new Map<string, string>();
  const repos: string[] = [];
  const addAgent = (
    parent: string,
    subagent: boolean,
    sessionId: string,
    role: string,
    title: string | null,
  ): string => {
    const index = steps.length;
    const agentId = `a${index}`;
    const id = `agent:${agentId}`;
    sessionOf.set(id, sessionId);
    const parentAgent = parent === "user" ? null : parent.slice("agent:".length);
    const parentIsAgent = parent !== "user";
    steps.push({
      node: {
        id,
        kind: "agent",
        label: subagent ? role : title === null ? "lead" : `worker ${index}`,
        parent_id: parent,
        agent: agent(
          agentId,
          sessionId,
          subagent ? parentAgent : null,
          subagent,
          role,
          random() < 0.3,
        ),
        session_id: sessionId,
        session_status: subagent ? null : random() < 0.3 ? "running" : "review",
        session_title: subagent ? null : (title ?? "Root session"),
        repo_id: null,
        repo_mode: null,
      },
      edges: parentIsAgent ? [edge(subagent ? "spawned" : "dispatched", parent, id, index)] : [],
    });
    agents.push(id);
    if (!subagent) leads.push(id);
    return id;
  };
  const root = addAgent("user", false, "s0", "lead", null);
  while (steps.length < nodeCount) {
    const index = steps.length;
    // Repos arrive early and spread out; the rest is three subagents to one child.
    if (repos.length < repoCount && index % 9 === 4) {
      const id = `repo:r${repos.length}`;
      repos.push(id);
      steps.push({
        node: {
          id,
          kind: "repo",
          label: `repo-${repos.length}`,
          parent_id: null,
          agent: null,
          session_id: null,
          session_status: null,
          session_title: null,
          repo_id: `r${repos.length}`,
          repo_mode: "write",
        },
        edges: [],
      });
    } else if (random() < 0.27) {
      addAgent(root, false, `s${index}`, "lead", `Child task ${index}`);
    } else {
      const parent = pick(leads);
      addAgent(
        parent,
        true,
        sessionOf.get(parent) ?? "s0",
        pick(["explorer", "reviewer", "tester"]),
        null,
      );
    }
  }
  // The other edges, each attached to the arrival of its later end.
  const position = new Map(steps.map((step, index) => [step.node.id, index]));
  const seen = new Set(steps.flatMap((step) => step.edges.map((one) => one.id)));
  let remaining = edgeCount - seen.size;
  let guard = 0;
  while (remaining > 0 && guard < edgeCount * 50) {
    guard += 1;
    const from = pick(agents);
    const roll = random();
    let made: GraphEdge;
    if (roll < 0.35 && repos.length > 0) {
      made = edge("changed", from, pick(repos), guard, 1 + Math.floor(random() * 6));
    } else if (roll < 0.6) {
      const parent = steps[position.get(from) ?? 0].node.parent_id;
      if (parent === null || parent === "user") continue;
      made = edge("report", from, parent, guard);
    } else {
      const to = pick(agents);
      if (to === from) continue;
      made = edge("message", from, to, guard, 1 + Math.floor(random() * 3));
    }
    if (seen.has(made.id)) continue;
    seen.add(made.id);
    const at = Math.max(position.get(made.from) ?? 0, position.get(made.to) ?? 0);
    steps[at].edges.push(made);
    remaining -= 1;
  }
  return steps;
}

/** The graph after the first `count` arrivals. */
export function graphAt(steps: readonly Step[], count: number): Graph {
  const shown = steps.slice(0, Math.max(1, Math.min(count, steps.length)));
  return {
    root_session_id: "s0",
    nodes: shown.map((step) => step.node),
    edges: shown.flatMap((step) => step.edges),
    from_ts: T0,
    to_ts: T0 + shown.length * 1000,
    at: T0 + shown.length * 1000,
  };
}
