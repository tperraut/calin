import type { EdgeKind } from "../../gen/EdgeKind";
import type { Graph } from "../../gen/Graph";
import type { GraphEdge } from "../../gen/GraphEdge";
import type { GraphNode } from "../../gen/GraphNode";
import { formatCost } from "../../lib/format";
import { agentLook, repoModeLabel, sessionLook, type Look } from "../../lib/status";

/** A node as the graph draws and names it. */
export type NodeView = {
  readonly id: string;
  readonly node: GraphNode;
  /** First line of the card. */
  readonly title: string;
  /** Second line: `lead`, a subagent's type, a child session's title, a repo's mode. Empty when there is none. */
  readonly role: string;
  /** The title and the role together, for a screen reader and for the details. */
  readonly name: string;
  /** For agents: the session's status for a lead, the agent's own otherwise. */
  readonly status: Look | undefined;
  /** For agents that cost something. */
  readonly cost: string | undefined;
  /** True for the agents of the session on screen. */
  readonly own: boolean;
};

/**
 * An edge as the graph draws it. `tree`: the line from a node to its parent in
 * the spawn tree, which carries the `spawned` or `dispatched` edge between the
 * two when there is one. `curve`: any other edge, drawn over the tree.
 */
export type EdgeView = {
  readonly id: string;
  readonly from: string;
  readonly to: string;
  readonly route: "tree" | "curve";
  /** Absent for a tree line the server gave no edge for (the user to the lead). */
  readonly edge: GraphEdge | undefined;
  readonly kind: EdgeKind | "link";
  readonly count: number;
  /** Position among the curves between the same two nodes. */
  readonly lane: number;
};

export type GraphModel = {
  readonly nodes: readonly NodeView[];
  readonly edges: readonly EdgeView[];
  readonly nodeById: ReadonlyMap<string, NodeView>;
  readonly edgeById: ReadonlyMap<string, EdgeView>;
};

const kindOrder: readonly EdgeKind[] = ["spawned", "dispatched", "message", "report", "changed"];

function nodeView(node: GraphNode, parent: GraphNode | undefined, openSessionId: string): NodeView {
  const title = node.label.trim() || node.id;
  let role = "";
  let status: Look | undefined;
  let cost: string | undefined;
  if (node.kind === "repo") {
    role = node.repo_mode === null ? "Repo" : repoModeLabel(node.repo_mode);
  } else if (node.kind === "agent" && node.agent) {
    const agent = node.agent;
    // A dispatched child's lead is known by what it was sent to do.
    const childLead = agent.kind !== "subagent" && parent?.kind === "agent";
    role = childLead && node.session_title ? node.session_title : agent.role;
    status =
      node.session_status === null ? agentLook(agent.status) : sessionLook(node.session_status);
    if (agent.cost_usd > 0) cost = formatCost(agent.cost_usd);
  }
  if (role === title) role = "";
  return {
    id: node.id,
    node,
    title,
    role,
    name: role === "" ? title : `${title} (${role})`,
    status,
    cost,
    own: node.kind === "agent" && node.session_id === openSessionId,
  };
}

/** What the graph draws, from what the server sent. Pure. */
export function graphModel(graph: Graph, openSessionId: string): GraphModel {
  const raw = new Map(graph.nodes.map((node) => [node.id, node]));
  const nodes = graph.nodes.map((node) =>
    nodeView(node, node.parent_id === null ? undefined : raw.get(node.parent_id), openSessionId),
  );
  const nodeById = new Map(nodes.map((node) => [node.id, node]));

  const edges: EdgeView[] = [];
  const carried = new Set<string>();
  const lanes = new Map<string, number>();
  const sorted = graph.edges
    .filter((edge) => nodeById.has(edge.from) && nodeById.has(edge.to))
    .toSorted((a, b) => kindOrder.indexOf(a.kind) - kindOrder.indexOf(b.kind));
  for (const edge of sorted) {
    const child = raw.get(edge.to);
    const structural =
      (edge.kind === "spawned" || edge.kind === "dispatched") &&
      child?.parent_id === edge.from &&
      !carried.has(edge.to);
    if (structural) {
      carried.add(edge.to);
      edges.push({ ...edge, route: "tree", edge, lane: 0 });
      continue;
    }
    const pair = edge.from < edge.to ? `${edge.from}\n${edge.to}` : `${edge.to}\n${edge.from}`;
    const lane = lanes.get(pair) ?? 0;
    lanes.set(pair, lane + 1);
    edges.push({ ...edge, route: "curve", edge, lane });
  }
  // Every node shows where it hangs, whether or not an edge says how it got there.
  for (const node of graph.nodes) {
    if (node.kind === "repo" || node.parent_id === null || carried.has(node.id)) continue;
    if (!nodeById.has(node.parent_id)) continue;
    edges.push({
      id: `link:${node.id}`,
      from: node.parent_id,
      to: node.id,
      route: "tree",
      edge: undefined,
      kind: "link",
      count: 1,
      lane: 0,
    });
  }
  return { nodes, edges, nodeById, edgeById: new Map(edges.map((edge) => [edge.id, edge])) };
}

const kindWords: Record<EdgeKind, string> = {
  spawned: "Spawned subagent",
  dispatched: "Dispatched child session",
  message: "Notes",
  report: "Report",
  changed: "Changed files",
};

/** An edge's kind in words. */
export const edgeKindWords = (kind: EdgeKind | "link"): string =>
  kind === "link" ? "Started" : (kindWords[kind] ?? kind);

const times = (count: number, one: string, many: string): string =>
  count === 1 ? `1 ${one}` : `${count} ${many}`;

/** One edge as a sentence: who did what to whom, how often. */
export function edgeSentence(edge: EdgeView, model: GraphModel): string {
  const from = model.nodeById.get(edge.from)?.name ?? edge.from;
  const to = model.nodeById.get(edge.to)?.name ?? edge.to;
  switch (edge.kind) {
    case "spawned":
      return `${from} spawned ${to}`;
    case "dispatched":
      return `${from} dispatched ${to}`;
    case "message":
      return `${from} sent ${times(edge.count, "note", "notes")} to ${to}`;
    case "report":
      return `${from} sent ${times(edge.count, "report", "reports")} to ${to}`;
    case "changed":
      return `${from} changed ${times(edge.count, "file", "files")} in ${to}`;
    case "link":
      return `${from} started ${to}`;
    default:
      return `${from} to ${to}: ${String(edge.kind)}, ${edge.count}`;
  }
}

/** One node as a line of text: its name, its status, what it cost, whether it is of the open session. */
export function nodeSentence(node: NodeView): string {
  const parts = [node.name];
  if (node.node.kind === "repo") parts[0] = `Repo ${node.name}`;
  if (node.status) parts.push(node.status.label);
  if (node.cost !== undefined) parts.push(node.cost);
  if (node.own) parts.push("in the open session");
  return parts.join(", ");
}

export type OutlineItem = {
  readonly id: string;
  readonly text: string;
  readonly children: readonly OutlineItem[];
};

/**
 * The graph as text: the spawn tree as nested items (who is under whom), the
 * repos, and one sentence per edge. What a screen reader gets in place of the drawing.
 */
export function describeGraph(
  model: GraphModel,
  tree: {
    order: readonly string[];
    children: ReadonlyMap<string, readonly string[]>;
    parent: ReadonlyMap<string, string>;
  },
): { tree: OutlineItem[]; repos: string[]; edges: string[] } {
  const item = (id: string): OutlineItem | undefined => {
    const node = model.nodeById.get(id);
    if (!node || node.node.kind === "repo") return undefined;
    return {
      id,
      text: nodeSentence(node),
      children: (tree.children.get(id) ?? []).flatMap((child) => item(child) ?? []),
    };
  };
  const roots = tree.order.filter(
    (id) => !tree.parent.has(id) && model.nodeById.get(id)?.node.kind !== "repo",
  );
  return {
    tree: roots.flatMap((id) => item(id) ?? []),
    repos: model.nodes.filter((node) => node.node.kind === "repo").map(nodeSentence),
    edges: model.edges
      .filter((edge) => edge.kind !== "link")
      .map((edge) => edgeSentence(edge, model)),
  };
}

/** Who changed a repo, and how many files each, most first. */
export function repoChangers(
  repoId: string,
  model: GraphModel,
): { edgeId: string; name: string; count: number }[] {
  return model.edges
    .filter((edge) => edge.kind === "changed" && edge.to === repoId)
    .map((edge) => ({
      edgeId: edge.id,
      name: model.nodeById.get(edge.from)?.name ?? edge.from,
      count: edge.count,
    }))
    .toSorted((a, b) => b.count - a.count);
}

/**
 * The edges to bring out, and whether the others are to be faded: the picked
 * edge alone; else every edge of the picked node, or of the focused one.
 */
export function edgeEmphasis(
  model: GraphModel,
  selection: { kind: "node" | "edge"; id: string } | undefined,
  focused: string | undefined,
): { active: boolean; hot: ReadonlySet<string> } {
  if (selection?.kind === "edge" && model.edgeById.has(selection.id)) {
    return { active: true, hot: new Set([selection.id]) };
  }
  const node = selection?.kind === "node" ? selection.id : focused;
  if (node === undefined || !model.nodeById.has(node)) return { active: false, hot: new Set() };
  return {
    active: true,
    hot: new Set(
      model.edges.filter((edge) => edge.from === node || edge.to === node).map((edge) => edge.id),
    ),
  };
}

/** The edges that touch a node, for its details: each can be selected from there. */
export const edgesOf = (nodeId: string, model: GraphModel): EdgeView[] =>
  model.edges.filter(
    (edge) => edge.kind !== "link" && (edge.from === nodeId || edge.to === nodeId),
  );

/** Cuts a label to what a card can hold; the whole of it is in the details and the text alternative. */
export function clip(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, Math.max(1, max - 1)).trimEnd()}…`;
}
