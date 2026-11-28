import type { Agent } from "../../gen/Agent";
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

/** What a node's card says under its title, and the agent's status and cost. */
type NodeFacts = Pick<NodeView, "role" | "status" | "cost">;

const noFacts: NodeFacts = { role: "", status: undefined, cost: undefined };

function agentFacts(node: GraphNode, agent: Agent, parent: GraphNode | undefined): NodeFacts {
  // A dispatched child's lead is known by what it was sent to do.
  const childLead = agent.kind !== "subagent" && parent?.kind === "agent";
  return {
    role: childLead && node.session_title ? node.session_title : agent.role,
    status:
      node.session_status === null ? agentLook(agent.status) : sessionLook(node.session_status),
    cost: agent.cost_usd > 0 ? formatCost(agent.cost_usd) : undefined,
  };
}

function nodeFacts(node: GraphNode, parent: GraphNode | undefined): NodeFacts {
  if (node.kind === "repo") {
    return { ...noFacts, role: node.repo_mode === null ? "Repo" : repoModeLabel(node.repo_mode) };
  }
  if (node.kind === "agent" && node.agent) return agentFacts(node, node.agent, parent);
  return noFacts;
}

function nodeView(node: GraphNode, parent: GraphNode | undefined, openSessionId: string): NodeView {
  const title = node.label.trim() || node.id;
  const facts = nodeFacts(node, parent);
  const role = facts.role === title ? "" : facts.role;
  return {
    id: node.id,
    node,
    title,
    role,
    name: role === "" ? title : `${title} (${role})`,
    status: facts.status,
    cost: facts.cost,
    own: node.kind === "agent" && node.session_id === openSessionId,
  };
}

/** True for the first spawn or dispatch of a node by its parent: it is drawn as the tree line. */
const carriesTree = (
  edge: GraphEdge,
  child: GraphNode | undefined,
  carried: ReadonlySet<string>,
): boolean =>
  (edge.kind === "spawned" || edge.kind === "dispatched") &&
  child?.parent_id === edge.from &&
  !carried.has(edge.to);

/** The same key for both directions between two nodes: their curves share lanes. */
const pairKey = (edge: GraphEdge): string =>
  edge.from < edge.to ? `${edge.from}\n${edge.to}` : `${edge.to}\n${edge.from}`;

/** Each edge as a tree line or a curve in its own lane, and the nodes a tree line reaches. */
function routeEdges(
  edges: readonly GraphEdge[],
  raw: ReadonlyMap<string, GraphNode>,
): { routed: EdgeView[]; carried: ReadonlySet<string> } {
  const routed: EdgeView[] = [];
  const carried = new Set<string>();
  const lanes = new Map<string, number>();
  for (const edge of edges) {
    if (carriesTree(edge, raw.get(edge.to), carried)) {
      carried.add(edge.to);
      routed.push({ ...edge, route: "tree", edge, lane: 0 });
      continue;
    }
    const pair = pairKey(edge);
    const lane = lanes.get(pair) ?? 0;
    lanes.set(pair, lane + 1);
    routed.push({ ...edge, route: "curve", edge, lane });
  }
  return { routed, carried };
}

/** The tree line from a node's parent when no edge carries it, so every node shows where it hangs. */
function linkEdge(
  node: GraphNode,
  carried: ReadonlySet<string>,
  held: ReadonlyMap<string, NodeView>,
): EdgeView[] {
  const parent = node.parent_id;
  if (node.kind === "repo" || parent === null || carried.has(node.id) || !held.has(parent)) {
    return [];
  }
  return [
    {
      id: `link:${node.id}`,
      from: parent,
      to: node.id,
      route: "tree",
      edge: undefined,
      kind: "link",
      count: 1,
      lane: 0,
    },
  ];
}

/** What the graph draws, from what the server sent. Pure. */
export function graphModel(graph: Graph, openSessionId: string): GraphModel {
  const raw = new Map(graph.nodes.map((node) => [node.id, node]));
  const nodes = graph.nodes.map((node) =>
    nodeView(node, node.parent_id === null ? undefined : raw.get(node.parent_id), openSessionId),
  );
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const sorted = graph.edges
    .filter((edge) => nodeById.has(edge.from) && nodeById.has(edge.to))
    .toSorted((a, b) => kindOrder.indexOf(a.kind) - kindOrder.indexOf(b.kind));
  const { routed, carried } = routeEdges(sorted, raw);
  const edges = [...routed, ...graph.nodes.flatMap((node) => linkEdge(node, carried, nodeById))];
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

/** A node's name, or its id when the model does not hold it. */
const nameOf = (id: string, model: GraphModel): string => model.nodeById.get(id)?.name ?? id;

type Sentence = (from: string, to: string, count: number) => string;

const sentences: Readonly<Record<EdgeKind | "link", Sentence>> = {
  spawned: (from, to) => `${from} spawned ${to}`,
  dispatched: (from, to) => `${from} dispatched ${to}`,
  message: (from, to, count) => `${from} sent ${times(count, "note", "notes")} to ${to}`,
  report: (from, to, count) => `${from} sent ${times(count, "report", "reports")} to ${to}`,
  changed: (from, to, count) => `${from} changed ${times(count, "file", "files")} in ${to}`,
  link: (from, to) => `${from} started ${to}`,
};

/** One edge as a sentence: who did what to whom, how often. */
export function edgeSentence(edge: EdgeView, model: GraphModel): string {
  const from = nameOf(edge.from, model);
  const to = nameOf(edge.to, model);
  // A kind from a newer server has no sentence yet: it is told by its wire name.
  const sentence: Sentence | undefined = sentences[edge.kind];
  return sentence
    ? sentence(from, to, edge.count)
    : `${from} to ${to}: ${String(edge.kind)}, ${edge.count}`;
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

/** A tree node and everything under it; nothing for a repo or a node the model does not hold. */
function outlineItem(
  id: string,
  model: GraphModel,
  children: ReadonlyMap<string, readonly string[]>,
): OutlineItem[] {
  const node = model.nodeById.get(id);
  if (!node || node.node.kind === "repo") return [];
  const below = (children.get(id) ?? []).flatMap((child) => outlineItem(child, model, children));
  return [{ id, text: nodeSentence(node), children: below }];
}

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
  return {
    tree: tree.order
      .filter((id) => !tree.parent.has(id))
      .flatMap((id) => outlineItem(id, model, tree.children)),
    repos: model.nodes.filter((node) => node.node.kind === "repo").map(nodeSentence),
    edges: model.edges
      .filter((edge) => edge.kind !== "link")
      .map((edge) => edgeSentence(edge, model)),
  };
}

/** The node of a session's lead: it carries the session's title and status, a subagent's node does not. */
export function sessionLead(sessionId: string | null, model: GraphModel): GraphNode | undefined {
  if (sessionId === null) return undefined;
  return model.nodes.find(
    (one) => one.node.session_id === sessionId && one.node.session_title !== null,
  )?.node;
}

/** Who changed a repo, and how many files each, most first. */
export function repoChangers(
  repoId: string,
  model: GraphModel,
): { edgeId: string; name: string; count: number }[] {
  return model.edges
    .filter((edge) => edge.kind === "changed" && edge.to === repoId)
    .map((edge) => ({ edgeId: edge.id, name: nameOf(edge.from, model), count: edge.count }))
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

/**
 * Cut in the middle: sibling sessions share their start ("Ship the health check:
 * part A", "… part B"), so the end is what tells them apart.
 */
export function clipMiddle(text: string, max: number): string {
  if (text.length <= max) return text;
  const tail = Math.floor((max - 1) / 2);
  const head = max - 1 - tail;
  return `${text.slice(0, head).trimEnd()}…${text.slice(text.length - tail).trimStart()}`;
}
