import type { WorkflowEdge } from "../../gen/WorkflowEdge";
import type { WorkflowNodeInfo } from "../../gen/WorkflowNodeInfo";
import type { WorkflowNodeRun } from "../../gen/WorkflowNodeRun";
import type { WorkflowRun } from "../../gen/WorkflowRun";
import { isFailurePort, kindWord, loopProgress } from "../../lib/workflow";

/** One node with where it stands in the run. */
export type NodeOverlay = {
  id: string;
  info: WorkflowNodeInfo;
  state: "idle" | "active" | "finished";
  runs: number;
  lastPort: string | null;
  /** True when the port it last left through is a failure port. */
  failed: boolean;
  outcome: string | null;
  /** "2 of 3" for a loop that has been entered. */
  loop: string | undefined;
};

/** One edge with how often the run went through it. */
export type EdgeOverlay = {
  index: number;
  edge: WorkflowEdge;
  failure: boolean;
  taken: number;
};

/** Where a port of a node leads, or that it ends the run. */
export type PortTarget = { port: string; failure: boolean; to: string | undefined };

export type RunOverlay = {
  nodes: readonly NodeOverlay[];
  byId: ReadonlyMap<string, NodeOverlay>;
  edges: readonly EdgeOverlay[];
  /** The node running now, if one is. */
  active: string | undefined;
};

const sameEdge = (a: WorkflowEdge, b: WorkflowEdge): boolean =>
  a.from === b.from && a.port === b.port && a.to === b.to;

const idle = (id: string): WorkflowNodeRun => ({
  id,
  state: "idle",
  runs: 0,
  last_port: null,
  outcome: null,
  iteration: null,
  max: null,
});

/** The run laid over its graph. Pure. */
export function runOverlay(run: WorkflowRun): RunOverlay {
  const runs = new Map(run.nodes.map((node) => [node.id, node]));
  const nodes = run.graph.nodes.map((info): NodeOverlay => {
    const known = runs.get(info.id) ?? idle(info.id);
    const iteration =
      known.iteration ?? (info.kind === "loop" && known.runs > 0 ? known.runs : null);
    return {
      id: info.id,
      info,
      // Widened on purpose: a state from a newer server reads as not run.
      state:
        known.state === "active" || known.state === "finished" ? known.state : ("idle" as const),
      runs: known.runs,
      lastPort: known.last_port,
      failed: known.last_port !== null && isFailurePort(info, known.last_port),
      outcome: known.outcome,
      loop:
        info.kind === "loop" && iteration !== null && iteration > 0
          ? loopProgress(iteration, known.max)
          : undefined,
    };
  });
  const byInfo = new Map(run.graph.nodes.map((node) => [node.id, node]));
  const edges = run.graph.edges.map((edge, index): EdgeOverlay => ({
    index,
    edge,
    failure: isFailurePort(byInfo.get(edge.from), edge.port),
    taken: run.taken.filter((one) => sameEdge(one, edge)).length,
  }));
  return {
    nodes,
    byId: new Map(nodes.map((node) => [node.id, node])),
    edges,
    active: nodes.find((node) => node.state === "active")?.id,
  };
}

/** Each port of a node and the node it leads to; `to` is absent when taking it ends the run. */
export function portTargets(run: WorkflowRun, nodeId: string): PortTarget[] {
  const info = run.graph.nodes.find((node) => node.id === nodeId);
  if (!info) return [];
  const ports = info.ports.map((port) => ({ port: port.name, failure: port.failure }));
  // An edge from a port the node does not list (a newer server) still says where it goes.
  for (const edge of run.graph.edges) {
    if (edge.from === nodeId && !ports.some((one) => one.port === edge.port)) {
      ports.push({ port: edge.port, failure: isFailurePort(info, edge.port) });
    }
  }
  return ports.map(({ port, failure }) => ({
    port,
    failure,
    to: run.graph.edges.find((edge) => edge.from === nodeId && edge.port === port)?.to,
  }));
}

/** Where a port leads, in words. */
export function portSentence(target: PortTarget): string {
  if (target.to !== undefined) return `${target.port} leads to ${target.to}`;
  return `${target.port} ends the run as ${target.failure ? "failed" : "succeeded"}`;
}

/** A gate or a question that is active is not working: it is waiting for the reader. */
export const waitsForYou = (node: NodeOverlay): boolean =>
  node.state === "active" && (node.info.kind === "gate" || node.info.kind === "ask");

/** Where a node stands, in words: "Not run yet", "Running now (run 2)", "Finished through fail, 3 runs". */
export function stateWords(node: NodeOverlay): string {
  const times = node.runs > 1 ? `, ${node.runs} runs` : "";
  if (waitsForYou(node)) return "Waiting for you";
  if (node.state === "active")
    return node.runs > 1 ? `Running now (run ${node.runs})` : "Running now";
  if (node.state === "finished") {
    return node.lastPort === null
      ? `Finished${times}`
      : `Finished through ${node.lastPort}${times}`;
  }
  return "Not run yet";
}

/** A node card's accessible name: its id, kind and state; the label is in the text alternative. */
export function nodeName(node: NodeOverlay): string {
  const loop = node.loop === undefined ? "" : `, ${node.loop}`;
  return `${node.id}, ${kindWord(node.info.kind)}: ${stateWords(node)}${loop}`;
}

/** The nodes the run went through, in order: "start → implement → tests → …". */
export function takenPath(run: WorkflowRun): string[] {
  if (run.taken.length === 0) {
    const started = run.nodes.find((node) => node.runs > 0);
    return started ? [started.id] : [];
  }
  return [run.taken[0].from, ...run.taken.map((edge) => edge.to)];
}

/** One line per node for the text alternative, in file order. */
export function nodeLines(run: WorkflowRun, overlay: RunOverlay): string[] {
  return overlay.nodes.map((node) => {
    const label = node.info.label.trim() === "" ? "" : ` "${node.info.label.trim()}"`;
    const loop = node.loop === undefined ? "" : ` Loop ${node.loop}.`;
    const ports = portTargets(run, node.id).map(portSentence);
    const leads = ports.length === 0 ? "" : ` ${ports.join("; ")}.`;
    return `${node.id}: ${kindWord(node.info.kind)}${label}. ${stateWords(node)}.${loop}${leads}`;
  });
}
