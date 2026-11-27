// ELK configurations compared by the spike. Shared by the page and by scripts/stability.ts.

import type { EdgeKind, GEdge, GNode } from "./gen.ts";
import { treeLayout } from "./treelayout.ts";

export interface Pos {
  x: number;
  y: number;
}

export interface Config {
  name: string;
  note: string;
  root: Record<string, string>;
  /** Pass the previous run's coordinates as node x/y (and seed new nodes next to their parent). */
  prevXY?: boolean;
  /** Pass the previous run's coordinates as the `elk.position` option. */
  prevPosition?: boolean;
  /** Per-node options for nodes that already had a position. */
  pinned?: Record<string, string>;
  /** Skip the FIRST/LAST layer constraints for the user and repository nodes. */
  noConstraints?: boolean;
  /** Only these edge kinds are given to ELK; the renderer draws the others as direct curves. */
  edgeKinds?: EdgeKind[];
  /** Not ELK at all: the hand-written incremental tree layout in treelayout.ts. */
  custom?: "top" | "center";
}

export const LAYER_GAP = 56;
export const NODE_GAP = 12;

const base: Record<string, string> = {
  "elk.algorithm": "layered",
  // Layers run left to right, so the long axis of a wide fan-out is vertical: a phone scrolls that way.
  "elk.direction": "RIGHT",
  "elk.spacing.nodeNode": String(NODE_GAP),
  "elk.layered.spacing.nodeNodeBetweenLayers": String(LAYER_GAP),
  "elk.spacing.edgeNode": "8",
  "elk.spacing.edgeEdge": "6",
  "elk.layered.spacing.edgeNodeBetweenLayers": "10",
  "elk.layered.spacing.edgeEdgeBetweenLayers": "6",
  "elk.edgeRouting": "ORTHOGONAL",
};

const modelOrder: Record<string, string> = {
  ...base,
  "elk.separateConnectedComponents": "false",
  "elk.layered.considerModelOrder.strategy": "NODES_AND_EDGES",
  "elk.layered.crossingMinimization.forceNodeModelOrder": "true",
};

const interactive: Record<string, string> = {
  ...base,
  "elk.separateConnectedComponents": "false",
  "elk.layered.cycleBreaking.strategy": "INTERACTIVE",
  "elk.layered.layering.strategy": "INTERACTIVE",
  "elk.layered.crossingMinimization.strategy": "INTERACTIVE",
};

export const CONFIGS: Config[] = [
  { name: "layered-default", note: "layered, no stability options", root: base },
  {
    name: "layered-nosep",
    note: "default + separateConnectedComponents=false",
    root: { ...base, "elk.separateConnectedComponents": "false" },
  },
  { name: "model-order", note: "considerModelOrder NODES_AND_EDGES + forceNodeModelOrder", root: modelOrder },
  {
    name: "model-order-leftup",
    note: "model order + Brandes-Koepf fixedAlignment LEFTUP",
    root: { ...modelOrder, "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP" },
  },
  {
    name: "model-order-linear",
    note: "model order + nodePlacement LINEAR_SEGMENTS",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "LINEAR_SEGMENTS" },
  },
  {
    name: "model-order-simplex",
    note: "model order + nodePlacement NETWORK_SIMPLEX",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX" },
  },
  {
    name: "model-order-simple",
    note: "model order + nodePlacement SIMPLE",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "SIMPLE" },
  },
  {
    name: "interactive",
    note: "cycle breaking, layering and crossing minimisation INTERACTIVE, previous x/y as input",
    root: interactive,
    prevXY: true,
  },
  {
    name: "interactive-leftup",
    note: "interactive + Brandes-Koepf fixedAlignment LEFTUP",
    root: { ...interactive, "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP" },
    prevXY: true,
  },
  {
    name: "interactive-placer",
    note: "interactive + nodePlacement INTERACTIVE",
    root: { ...interactive, "elk.layered.nodePlacement.strategy": "INTERACTIVE" },
    prevXY: true,
  },
  {
    name: "semi-interactive",
    note: "layer sweep with semiInteractive=true and elk.position from the previous run",
    root: { ...base, "elk.separateConnectedComponents": "false", "elk.layered.crossingMinimization.semiInteractive": "true" },
    prevPosition: true,
  },
  {
    name: "mrtree",
    note: "ELK Mr. Tree on the same graph (non-tree edges are not routed around nodes)",
    root: { "elk.algorithm": "mrtree", "elk.direction": "RIGHT", "elk.spacing.nodeNode": String(NODE_GAP), "elk.mrtree.edgeRoutingMode": "AVOID_OVERLAP" },
    noConstraints: true,
  },
  {
    name: "stress-fixed",
    note: "ELK stress, existing nodes fixed (elk.stress.fixed), new node seeded next to its parent",
    root: { "elk.algorithm": "stress", "elk.interactive": "true", "elk.stress.desiredEdgeLength": "150" },
    prevXY: true,
    pinned: { "elk.stress.fixed": "true" },
    noConstraints: true,
  },
  {
    name: "tree-default",
    note: "layered defaults, but only spawned edges go to ELK",
    root: { ...base, "elk.separateConnectedComponents": "false" },
    edgeKinds: ["spawned"],
  },
  { name: "tree-model-order", note: "model order, only spawned edges go to ELK", root: modelOrder, edgeKinds: ["spawned"] },
  {
    name: "tree-model-order-leftup",
    note: "model order + BK LEFTUP, only spawned edges go to ELK",
    root: { ...modelOrder, "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP" },
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-model-order-linear",
    note: "model order + LINEAR_SEGMENTS, only spawned edges go to ELK",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "LINEAR_SEGMENTS" },
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-model-order-simplex",
    note: "model order + NETWORK_SIMPLEX, only spawned edges go to ELK",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX" },
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-model-order-simple",
    note: "model order + SIMPLE placement, only spawned edges go to ELK",
    root: { ...modelOrder, "elk.layered.nodePlacement.strategy": "SIMPLE" },
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-interactive",
    note: "interactive strategies with previous x/y, only spawned edges go to ELK",
    root: interactive,
    prevXY: true,
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-changed-model-order-leftup",
    note: "model order + BK LEFTUP, spawned and changed edges go to ELK",
    root: { ...modelOrder, "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP" },
    edgeKinds: ["spawned", "changed"],
  },
  {
    name: "mrtree-tree",
    note: "ELK Mr. Tree, only spawned edges",
    root: { "elk.algorithm": "mrtree", "elk.direction": "RIGHT", "elk.spacing.nodeNode": String(NODE_GAP) },
    noConstraints: true,
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-model-order-nocm",
    note: "model order + BK LEFTUP, crossing minimisation NONE and greedy switch OFF, only spawned edges",
    root: {
      ...modelOrder,
      "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP",
      "elk.layered.crossingMinimization.strategy": "NONE",
      "elk.layered.crossingMinimization.greedySwitch.type": "OFF",
    },
    edgeKinds: ["spawned"],
  },
  {
    name: "tree-order-noforce",
    note: "considerModelOrder without forceNodeModelOrder, BK LEFTUP, only spawned edges",
    root: {
      ...base,
      "elk.separateConnectedComponents": "false",
      "elk.layered.considerModelOrder.strategy": "NODES_AND_EDGES",
      "elk.layered.nodePlacement.bk.fixedAlignment": "LEFTUP",
    },
    edgeKinds: ["spawned"],
  },
  { name: "custom-tree-top", note: "no ELK: hand-written tree, parent aligned with its first child", root: {}, custom: "top" },
  { name: "custom-tree-center", note: "no ELK: hand-written tree, parent centred on its children", root: {}, custom: "center" },
];

export function configByName(name: string): Config {
  const c = CONFIGS.find((c) => c.name === name);
  if (!c) throw new Error(`unknown ELK config ${name}`);
  return c;
}

export interface ElkInput {
  id: string;
  layoutOptions: Record<string, string>;
  children: { id: string; width: number; height: number; x?: number; y?: number; layoutOptions?: Record<string, string> }[];
  edges: { id: string; sources: string[]; targets: string[] }[];
}

export function toElk(nodes: GNode[], edges: GEdge[], config: Config, prev: Map<string, Pos>): ElkInput {
  const seeded = new Map<string, Pos>();
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const children = nodes.map((n) => {
    const options: Record<string, string> = {};
    if (!config.noConstraints) {
      if (n.kind === "user") options["elk.layered.layering.layerConstraint"] = "FIRST";
      if (n.kind === "repo") options["elk.layered.layering.layerConstraint"] = "LAST";
    }
    let at = prev.get(n.id);
    if (at && config.pinned) Object.assign(options, config.pinned);
    if (!at && (config.prevXY || config.prevPosition)) {
      // A new node starts one layer to the right of its parent, below everything already there.
      const parent = n.parent ? byId.get(n.parent) : undefined;
      const p = parent ? (prev.get(parent.id) ?? seeded.get(parent.id)) : undefined;
      if (parent && p) {
        const x = p.x + parent.w + LAYER_GAP;
        let y = p.y;
        for (const other of nodes) {
          const q = prev.get(other.id) ?? seeded.get(other.id);
          if (q && Math.abs(q.x - x) < 60) y = Math.max(y, q.y + other.h + NODE_GAP);
        }
        at = { x, y };
      } else {
        at = { x: n.kind === "repo" ? 900 : 0, y: seeded.size * 50 };
      }
      seeded.set(n.id, at);
    }
    const child: ElkInput["children"][number] = { id: n.id, width: n.w, height: n.h };
    if (at && config.prevXY) {
      child.x = at.x;
      child.y = at.y;
    }
    if (at && config.prevPosition) options["elk.position"] = `(${at.x},${at.y})`;
    if (Object.keys(options).length) child.layoutOptions = options;
    return child;
  });
  return {
    id: "root",
    layoutOptions: config.root,
    children,
    edges: edges
      .filter((e) => !config.edgeKinds || config.edgeKinds.includes(e.kind))
      .map((e) => ({ id: e.id, sources: [e.from], targets: [e.to] })),
  };
}

export interface LayoutOut {
  pos: Map<string, Pos>;
  /** Routes for the edges the layout routed, as flattened x,y pairs. The rest are drawn direct. */
  routes: Map<string, Float32Array>;
  w: number;
  h: number;
  /** Time ELK reports for the layout itself, without messaging. */
  elkMs?: number;
}

export interface ElkLike {
  layout(graph: unknown, args?: unknown): Promise<any>;
}

export async function runLayout(
  elk: ElkLike | null,
  nodes: GNode[],
  edges: GEdge[],
  config: Config,
  prev: Map<string, Pos>,
  measure = false,
): Promise<LayoutOut> {
  if (config.custom) return treeLayout(nodes, config.custom);
  if (!elk) throw new Error("this configuration needs ELK");
  // measureExecutionTime makes some algorithms (mrtree, the interactive strategies) throw a
  // NullPointerException in elkjs 0.12.2, so it is opt-in and falls back to a plain call.
  const graph = toElk(nodes, edges, config, prev);
  let out: any;
  try {
    out = await elk.layout(graph, measure ? { measureExecutionTime: true } : undefined);
  } catch (error) {
    if (!measure) throw error;
    out = await elk.layout(toElk(nodes, edges, config, prev));
  }
  const pos = new Map<string, Pos>();
  let w = out.width ?? 0, h = out.height ?? 0;
  for (const c of out.children) {
    pos.set(c.id, { x: c.x, y: c.y });
    w = Math.max(w, c.x + c.width);
    h = Math.max(h, c.y + c.height);
  }
  const routes = new Map<string, Float32Array>();
  for (const e of out.edges ?? []) {
    const s = e.sections?.[0];
    if (!s) continue;
    const pts = [s.startPoint, ...(s.bendPoints ?? []), s.endPoint];
    const flat = new Float32Array(pts.length * 2);
    pts.forEach((p: Pos, i: number) => {
      flat[i * 2] = p.x;
      flat[i * 2 + 1] = p.y;
    });
    routes.set(e.id, flat);
  }
  const elkMs = typeof out.logging?.executionTime === "number" ? out.logging.executionTime * 1000 : undefined;
  return { pos, routes, w, h, elkMs };
}
