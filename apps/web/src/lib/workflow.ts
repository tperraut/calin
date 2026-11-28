import type { EventBody } from "../gen/EventBody";
import type { WorkflowEdge } from "../gen/WorkflowEdge";
import type { WorkflowGraph } from "../gen/WorkflowGraph";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import type { WorkflowNodeInfo } from "../gen/WorkflowNodeInfo";
import type { WorkflowNodeRun } from "../gen/WorkflowNodeRun";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WorkflowStatus } from "../gen/WorkflowStatus";
import type { WorkflowNote } from "../state/sessionReducer";
import { compareText, counted } from "./format";

/** The workflow a session runs when it names none (workflows spec, section 1). */
export const QUICK_WORKFLOW = "quick";

/** The workflow id a session is started with; `quick` is said by saying nothing. */
export const sentWorkflow = (picked: WorkflowInfo | undefined): string | undefined =>
  picked?.id === QUICK_WORKFLOW ? undefined : picked?.id;

/** Node types in the order of the spec's table (section 3); a newer one sorts last. */
export const NODE_KINDS = [
  "start",
  "end",
  "agent",
  "check",
  "loop",
  "gate",
  "ask",
  "parallel",
  "commit",
  "ship",
] as const;

type KnownKind = (typeof NODE_KINDS)[number];

const isKnown = (kind: string): kind is KnownKind =>
  (NODE_KINDS as readonly string[]).includes(kind);

/** How each kind is named in sentences, and its icon. The word always goes with the icon. */
const kindLooks: Record<KnownKind, { word: string; icon: string }> = {
  start: { word: "Start", icon: "▶" },
  end: { word: "End", icon: "■" },
  agent: { word: "Agent step", icon: "✦" },
  check: { word: "Check", icon: "✓" },
  loop: { word: "Loop", icon: "↻" },
  gate: { word: "Gate", icon: "⏸" },
  ask: { word: "Question", icon: "?" },
  parallel: { word: "Parallel", icon: "⇉" },
  commit: { word: "Commit", icon: "●" },
  ship: { word: "Ship", icon: "⇪" },
};

export const kindWord = (kind: string): string => (isKnown(kind) ? kindLooks[kind].word : kind);
export const kindIcon = (kind: string): string => (isKnown(kind) ? kindLooks[kind].icon : "◇");

/** A node that is not a step of the work: the run's own ends. */
export const isStep = (kind: string): boolean => kind !== "start" && kind !== "end";

/** A kind's place in the spec's table; a kind it does not list comes after all of them. */
function kindRank(kind: string): number {
  const index = (NODE_KINDS as readonly string[]).indexOf(kind);
  return index === -1 ? NODE_KINDS.length : index;
}

const byKind = ([a]: [string, number], [b]: [string, number]): number =>
  kindRank(a) - kindRank(b) || compareText(a, b);

/** "5 steps: 2 agent, 1 check, 1 gate, 1 ship": every node but `start` and `end`, by kind. */
export function shapeSummary(graph: WorkflowGraph): string {
  const counts = new Map<string, number>();
  for (const node of graph.nodes) {
    if (isStep(node.kind)) counts.set(node.kind, (counts.get(node.kind) ?? 0) + 1);
  }
  if (counts.size === 0) return "No steps";
  const parts = [...counts].toSorted(byKind).map(([kind, count]) => `${count} ${kind}`);
  return `${counted(stepCount(graph), "step", "steps")}: ${parts.join(", ")}`;
}

/** Number of nodes that are steps (see `isStep`). */
export const stepCount = (graph: WorkflowGraph): number =>
  graph.nodes.filter((node) => isStep(node.kind)).length;

/** True when a workflow can be chosen for a new session. */
export const isUsable = (workflow: WorkflowInfo): boolean =>
  workflow.graph !== null && workflow.problem === null;

/** The problems of a workflow file, one per line, as the server lists them. */
export const problemLines = (workflow: WorkflowInfo): string[] =>
  (workflow.problem ?? "")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.trim() !== "");

/** Dollars to the cent, for budgets: "$1.20". */
export const dollars = (usd: number): string => `$${usd.toFixed(2)}`;

/** The edge a node leaves through `port`, if one is wired. */
export const edgeOf = (
  graph: WorkflowGraph,
  nodeId: string,
  port: string,
): WorkflowEdge | undefined =>
  graph.edges.find((edge) => edge.from === nodeId && edge.port === port);

/** True when `port` of `node` is a failure port (spec section 3). */
export function isFailurePort(node: WorkflowNodeInfo | undefined, port: string): boolean {
  const known = node?.ports.find((one) => one.name === port);
  if (known) return known.failure;
  return ["failed", "fail", "exhausted", "reject"].includes(port);
}

/** A node that has not run yet. */
export const idleRun = (id: string): WorkflowNodeRun => ({
  id,
  state: "idle",
  runs: 0,
  last_port: null,
  outcome: null,
  iteration: null,
  max: null,
});

type Started = Extract<EventBody, { type: "workflow_started" }>;
type NodeEvent = Extract<EventBody, { type: "workflow_node" }>;
type Finished = Extract<EventBody, { type: "workflow_finished" }>;

/** The run as `workflow_started` begins it: every node idle. */
export function startRun(body: Started): WorkflowRun {
  // Widened on purpose: a graph missing from a malformed event must not throw in the fold.
  const graph = (body.graph as WorkflowGraph | null | undefined) ?? { nodes: [], edges: [] };
  return {
    workflow_id: body.workflow_id,
    name: body.name,
    graph,
    nodes: graph.nodes.map((node) => idleRun(node.id)),
    taken: [],
    status: "running",
    reason: null,
    budget_usd: body.budget_usd ?? null,
    cost_usd: 0,
  };
}

/** A node started or finished (workflows spec, section 6). Pure; an unknown node changes nothing. */
export function foldNode(run: WorkflowRun, body: NodeEvent): WorkflowRun {
  const index = run.nodes.findIndex((node) => node.id === body.node_id);
  if (index === -1) return run;
  const node = run.nodes[index];
  if (body.phase === "started") {
    const runs = node.runs + 1;
    // A loop counts its entries up to its limit (spec section 3); the limit comes from the server.
    const iteration = node.max === null ? node.iteration : Math.min(runs, node.max);
    return {
      ...run,
      nodes: run.nodes.with(index, { ...node, state: "active", runs, iteration }),
    };
  }
  if (body.phase !== "finished") return run;
  const port = body.port;
  const edge = port === null ? undefined : edgeOf(run.graph, body.node_id, port);
  return {
    ...run,
    nodes: run.nodes.with(index, {
      ...node,
      state: "finished",
      last_port: port ?? node.last_port,
      outcome: body.outcome,
    }),
    taken: edge ? [...run.taken, edge] : run.taken,
  };
}

/** The run ended: nothing is active any more. */
export function finishRun(run: WorkflowRun, body: Finished): WorkflowRun {
  return {
    ...run,
    status: body.status,
    reason: body.reason,
    nodes: run.nodes.map((node) =>
      node.state === "active" ? { ...node, state: "finished" } : node,
    ),
  };
}

/**
 * The run on screen: the one the events folded, with what only the server
 * knows (each loop's limit, the session tree's cost) from its last answer.
 * Without events (a replay still on its way), the server's answer as it is.
 */
export function mergeRun(
  folded: WorkflowRun | null,
  served: WorkflowRun | undefined,
): WorkflowRun | undefined {
  if (!folded) return served;
  if (!served || served.workflow_id !== folded.workflow_id) return folded;
  const byId = new Map(served.nodes.map((node) => [node.id, node]));
  return {
    ...folded,
    budget_usd: folded.budget_usd ?? served.budget_usd,
    cost_usd: served.cost_usd,
    nodes: folded.nodes.map((node) => mergeNode(node, byId.get(node.id))),
  };
}

/** One node of `mergeRun`: the loop limit from the server, the entries from the events. */
function mergeNode(node: WorkflowNodeRun, known: WorkflowNodeRun | undefined): WorkflowNodeRun {
  const max = known?.max ?? node.max;
  if (max === null) return { ...node, iteration: known?.iteration ?? node.iteration };
  // The events count entries as they come; the server's count may be a moment behind.
  return { ...node, max, iteration: Math.min(node.runs, max) };
}

const statusWords: Record<WorkflowStatus, string> = {
  running: "Running",
  success: "Succeeded",
  failed: "Failed",
};

export const runStatusWords = (status: WorkflowStatus): string => statusWords[status] ?? status;

/** "2 of 3" for a loop whose limit is known, "entered 2 times" otherwise. */
export function loopProgress(iteration: number, max: number | null): string {
  if (max !== null) return `${Math.min(iteration, max)} of ${max}`;
  return iteration === 1 ? "entered once" : `entered ${iteration} times`;
}

/** A workflow row of the transcript, as words: the line itself, and what follows it in a lighter voice. */
export type NoteWords = {
  text: string;
  detail: string | null;
  tone: "neutral" | "danger" | "success";
};

/** What a `node_started` row says, by kind: "Check: cargo test", "Loop: attempt 2 of 3". */
function startWords(
  kind: string,
  nodeId: string,
  label: string,
  run: number,
  max: number | null,
): NoteWords {
  const about = label.trim();
  switch (kind) {
    case "agent":
    case "parallel":
      // The id names the step; the label (a prompt's start) follows it.
      return { text: `${kindWord(kind)}: ${nodeId}`, detail: about || null, tone: "neutral" };
    case "loop":
      return { text: loopStartText(run, max), detail: null, tone: "neutral" };
    default:
      return { text: `${kindWord(kind)}: ${about || nodeId}`, detail: null, tone: "neutral" };
  }
}

/** "Loop: attempt 2 of 3", or that none is left once the limit is passed. */
function loopStartText(run: number, max: number | null): string {
  if (max === null) return `Loop: attempt ${run}`;
  if (run > max) return `Loop: no attempt left (${max} of ${max} used)`;
  return `Loop: attempt ${run} of ${max}`;
}

type Tone = NoteWords["tone"];

/** A verdict's outcome is often the verdict itself: said once is enough. */
function finishDetail(outcome: string | null, port: string): string | null {
  const said = outcome?.trim() || null;
  return said === port ? null : said;
}

function gateFinish(port: string, detail: string | null, tone: Tone): NoteWords {
  if (port === "approve") return { text: "Gate approved", detail, tone: "success" };
  if (port === "reject") return { text: "Gate rejected", detail, tone: "danger" };
  return { text: `Gate: ${port}`, detail, tone };
}

function agentFinish(nodeId: string, port: string, detail: string | null, tone: Tone): NoteWords {
  return port === "failed"
    ? { text: `Agent step failed: ${nodeId}`, detail, tone: "danger" }
    : { text: `Verdict of ${nodeId}: ${port}`, detail, tone };
}

function commitFinish(nodeId: string, port: string, detail: string | null, tone: Tone): NoteWords {
  return port === "nothing"
    ? { text: "Commit: nothing to commit", detail, tone: "neutral" }
    : { text: `Commit failed: ${nodeId}`, detail, tone };
}

/** What a `node_finished` row says: only results that are news get one (see the reducer). */
function finishWords(
  kind: string,
  nodeId: string,
  port: string,
  outcome: string | null,
  failure: boolean,
): NoteWords {
  const detail = finishDetail(outcome, port);
  const tone = failure ? "danger" : "neutral";
  switch (kind) {
    case "check":
      return { text: `Check failed: ${nodeId}`, detail, tone: "danger" };
    case "gate":
      return gateFinish(port, detail, tone);
    case "loop":
      return { text: `Loop ${nodeId}: no attempt left`, detail, tone: "danger" };
    case "agent":
      return agentFinish(nodeId, port, detail, tone);
    case "commit":
      return commitFinish(nodeId, port, detail, tone);
    default:
      return { text: `${kindWord(kind)} ${nodeId} left through ${port}`, detail, tone };
  }
}

/** The words of one workflow row of the transcript. `max` is the node's loop limit, when known. */
export function noteWords(note: WorkflowNote, max: number | null = null): NoteWords {
  switch (note.what) {
    case "started":
      return { text: `Workflow: ${note.name}`, detail: null, tone: "neutral" };
    case "node_started":
      return startWords(note.nodeKind, note.nodeId, note.label, note.run, max);
    case "node_finished":
      return finishWords(note.nodeKind, note.nodeId, note.port, note.outcome, note.failure);
    case "finished":
      return note.status === "success"
        ? { text: "Workflow succeeded", detail: null, tone: "success" }
        : note.status === "failed"
          ? { text: "Workflow failed", detail: note.reason, tone: "danger" }
          : { text: `Workflow ${note.status}`, detail: note.reason, tone: "neutral" };
  }
}
