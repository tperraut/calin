import type { WorkflowEdge } from "../../gen/WorkflowEdge";
import type { WorkflowGraph } from "../../gen/WorkflowGraph";
import type { Point, Size } from "./layout";

/**
 * `across`: layers left to right, for a pane with room. `down`: layers top to
 * bottom, for a phone.
 */
export type Orientation = "across" | "down";

/** Below this pane width the drawing runs top to bottom. */
export const ACROSS_MIN_WIDTH = 560;

/** No card is shorter than a 44px touch target; three lines of text fit. */
export const WF_NODE: Size = { w: 168, h: 60 };

const PAD = 16;
/** Room between two layers: an edge's port name is written there, next to where it leaves. */
const LAYER_GAP_ACROSS = 96;
const LAYER_GAP_DOWN = 64;
/** Room between two nodes of one layer. */
const SLOT_GAP_ACROSS = 32;
const SLOT_GAP_DOWN = 24;
/** Where the first back edge runs outside the nodes, and how far apart the next ones run. */
const LANE_OFFSET = 24;
const LANE_STEP = 16;
const CORNER = 8;

export type PlacedNode = {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  layer: number;
  /** Position within the layer, from 0. */
  slot: number;
};

export type PlacedEdge = {
  /** Index of the edge in the graph's `edges`: its id. */
  index: number;
  edge: WorkflowEdge;
  /** True when it closes a cycle (see `backEdges`); drawn around the outside. */
  back: boolean;
  /** SVG path data, from where it leaves to the side of the node it enters. */
  d: string;
  /** Where its port name is written, near its start. */
  label: Point & { anchor: "start" | "end" };
  /** Where a traversal count sits. */
  middle: Point;
};

export type WorkflowLayout = {
  orientation: Orientation;
  /** In the graph's file order. */
  nodes: readonly PlacedNode[];
  byId: ReadonlyMap<string, PlacedNode>;
  /** Only edges between two nodes of the graph. */
  edges: readonly PlacedEdge[];
  /** Layer by layer, each in file order: the order the keyboard moves in. */
  order: readonly string[];
  layers: readonly (readonly string[])[];
  width: number;
  height: number;
};

const round = (value: number): number => Math.round(value * 10) / 10;

/**
 * The edges that close a cycle in file order: one that enters a node declared
 * no later than the one it leaves, when that node leads back to it. Every
 * cycle has one (a cycle cannot keep going forward in the file), so the rest
 * is acyclic. Indices into `graph.edges`.
 */
export function backEdges(graph: WorkflowGraph): ReadonlySet<number> {
  const position = new Map(graph.nodes.map((node, index) => [node.id, index]));
  const next = new Map<string, string[]>();
  for (const edge of graph.edges) {
    next.set(edge.from, [...(next.get(edge.from) ?? []), edge.to]);
  }
  const back = new Set<number>();
  graph.edges.forEach((edge, index) => {
    if (closesCycle(edge, position, next)) back.add(index);
  });
  return back;
}

/** True when `edge` goes back in file order and its target leads back to its source. */
function closesCycle(
  edge: WorkflowEdge,
  position: ReadonlyMap<string, number>,
  next: ReadonlyMap<string, readonly string[]>,
): boolean {
  const from = position.get(edge.from);
  const to = position.get(edge.to);
  return from !== undefined && to !== undefined && to <= from && reaches(next, edge.to, edge.from);
}

/** True when a path along `next` goes from `from` to `to`. */
function reaches(next: ReadonlyMap<string, readonly string[]>, from: string, to: string): boolean {
  const seen = new Set<string>([from]);
  const stack = [from];
  while (stack.length > 0) {
    const at = stack.pop() as string;
    if (at === to) return true;
    const unseen = (next.get(at) ?? []).filter((one) => !seen.has(one));
    for (const one of unseen) seen.add(one);
    stack.push(...unseen);
  }
  return false;
}

/**
 * Each node's layer: the longest path to it from `start` (from any node
 * nothing enters, for a graph without one), over the edges that are not back edges.
 */
export function layersOf(
  graph: WorkflowGraph,
  back: ReadonlySet<number> = backEdges(graph),
): ReadonlyMap<string, number> {
  const ids = new Set(graph.nodes.map((node) => node.id));
  const forward = graph.edges.filter(
    (edge, index) => !back.has(index) && ids.has(edge.from) && ids.has(edge.to),
  );
  return longestPaths(
    graph.nodes.map((node) => node.id),
    forward,
  );
}

/** The longest path to each of `ids` over `forward`, which is acyclic. */
function longestPaths(
  ids: readonly string[],
  forward: readonly WorkflowEdge[],
): ReadonlyMap<string, number> {
  const incoming = new Map(ids.map((id) => [id, 0]));
  for (const edge of forward) bump(incoming, edge.to, 1);
  const layer = new Map(ids.map((id) => [id, 0]));
  // Kahn's order, ready nodes taken in file order, so the result never depends on anything else.
  const ready = ids.filter((id) => incoming.get(id) === 0);
  while (ready.length > 0) {
    const at = ready.shift() as string;
    for (const edge of forward.filter((one) => one.from === at)) {
      layer.set(edge.to, Math.max(countOf(layer, edge.to), countOf(layer, at) + 1));
      if (bump(incoming, edge.to, -1) === 0) ready.push(edge.to);
    }
  }
  return layer;
}

const countOf = (counts: ReadonlyMap<string, number>, id: string): number => counts.get(id) ?? 0;

/** Adds `by` to the count of `id`; the new count. */
function bump(counts: Map<string, number>, id: string, by: number): number {
  const next = countOf(counts, id) + by;
  counts.set(id, next);
  return next;
}

/** A path through `points` with rounded corners; every segment is horizontal or vertical. */
function orthogonal(points: readonly Point[]): string {
  let d = `M${round(points[0].x)},${round(points[0].y)}`;
  for (let index = 1; index < points.length - 1; index += 1) {
    const before = points[index - 1];
    const at = points[index];
    const after = points[index + 1];
    const inLength = Math.hypot(at.x - before.x, at.y - before.y);
    const outLength = Math.hypot(after.x - at.x, after.y - at.y);
    const r = Math.min(CORNER, inLength / 2, outLength / 2);
    const inX = at.x - (Math.sign(at.x - before.x) * r || 0);
    const inY = at.y - (Math.sign(at.y - before.y) * r || 0);
    const outX = at.x + (Math.sign(after.x - at.x) * r || 0);
    const outY = at.y + (Math.sign(after.y - at.y) * r || 0);
    d += `L${round(inX)},${round(inY)}Q${round(at.x)},${round(at.y)},${round(outX)},${round(outY)}`;
  }
  const last = points[points.length - 1];
  return `${d}L${round(last.x)},${round(last.y)}`;
}

function cubic(x0: number, y0: number, x1: number, y1: number, across: boolean): string {
  const reach = across ? Math.max(24, (x1 - x0) / 2) : Math.max(24, (y1 - y0) / 2);
  const [c1x, c1y, c2x, c2y] = across
    ? [x0 + reach, y0, x1 - reach, y1]
    : [x0, y0 + reach, x1, y1 - reach];
  return (
    `M${round(x0)},${round(y0)}C${round(c1x)},${round(c1y)},${round(c2x)},${round(c2y)},` +
    `${round(x1)},${round(y1)}`
  );
}

/**
 * Lays a workflow out in layers (see `layersOf`), each layer's nodes in file
 * order and centred on the widest layer. Edges leave a node from the side that
 * faces the next layer, one place per port in port order, and enter the
 * middle of the opposite side; a back edge goes out into the gap after its
 * node's layer, runs around the outside of the drawing in a lane of its own,
 * and comes back through the gap before its target's layer. Pure and deterministic.
 */
export function layoutWorkflow(graph: WorkflowGraph, orientation: Orientation): WorkflowLayout {
  const back = backEdges(graph);
  const layers = layerLists(graph, layersOf(graph, back));
  const frame = frameOf(orientation, Math.max(0, ...layers.map((layer) => layer.length)));
  const placed = placeNodes(layers, frame);
  const nodes = graph.nodes.flatMap((node) => {
    const one = placed.get(node.id);
    return one ? [one] : [];
  });
  const drawn = graph.edges.flatMap((edge, index) => {
    const from = placed.get(edge.from);
    const to = placed.get(edge.to);
    return from && to ? [{ edge, index, from, to }] : [];
  });
  const leaving = leavingOrder(graph, drawn);
  // Each back edge runs in a lane of its own, in edge order.
  const lanes = new Map(
    drawn.filter(({ index }) => back.has(index)).map(({ index }, lane) => [index, lane]),
  );
  const edges = drawn.map((one) => placeEdge(one, leaving, lanes.get(one.index), frame));
  return {
    orientation,
    nodes,
    byId: placed,
    edges,
    order: layers.flat(),
    layers,
    ...drawingSize(frame, layers.length, back.size),
  };
}

/** The nodes of each layer, in file order. */
function layerLists(graph: WorkflowGraph, layerOf: ReadonlyMap<string, number>): string[][] {
  const layerCount = Math.max(0, ...layerOf.values()) + (graph.nodes.length > 0 ? 1 : 0);
  const layers: string[][] = Array.from({ length: layerCount }, () => []);
  for (const node of graph.nodes) layers[layerOf.get(node.id) ?? 0].push(node.id);
  return layers;
}

/** The steps and gaps of a drawing in one orientation, with `widest` nodes in its widest layer. */
type Frame = {
  across: boolean;
  /** From one layer to the next. */
  layerStep: number;
  /** From one node of a layer to the next. */
  slotStep: number;
  layerGap: number;
  widest: number;
  /** The far side of the nodes: back edges run beyond it. */
  nodesEnd: number;
};

function frameOf(orientation: Orientation, widest: number): Frame {
  const across = orientation === "across";
  const { w, h } = WF_NODE;
  const layerGap = across ? LAYER_GAP_ACROSS : LAYER_GAP_DOWN;
  const slotGap = across ? SLOT_GAP_ACROSS : SLOT_GAP_DOWN;
  const slotStep = (across ? h : w) + slotGap;
  return {
    across,
    layerStep: (across ? w : h) + layerGap,
    slotStep,
    layerGap,
    widest,
    nodesEnd: PAD + Math.max(0, widest * slotStep - slotGap),
  };
}

/** Each layer's nodes one after the other, the layer centred on the widest one. */
function placeNodes(layers: readonly (readonly string[])[], frame: Frame): Map<string, PlacedNode> {
  const { w, h } = WF_NODE;
  const placed = new Map<string, PlacedNode>();
  layers.forEach((layer, index) => {
    const offset = ((frame.widest - layer.length) * frame.slotStep) / 2;
    layer.forEach((id, slot) => {
      const along = PAD + index * frame.layerStep;
      const side = PAD + offset + slot * frame.slotStep;
      const [x, y] = frame.across ? [along, side] : [side, along];
      placed.set(id, { id, x, y, w, h, layer: index, slot });
    });
  });
  return placed;
}

/** An edge between two placed nodes, with both of them. */
type DrawnEdge = { edge: WorkflowEdge; index: number; from: PlacedNode; to: PlacedNode };

/** The rank of an edge's port among its node's ports; a port the node does not list goes last. */
function portRank(graph: WorkflowGraph, edge: WorkflowEdge): number {
  const node = graph.nodes.find((one) => one.id === edge.from);
  const rank = node?.ports.findIndex((port) => port.name === edge.port) ?? -1;
  return rank === -1 ? Number.MAX_SAFE_INTEGER : rank;
}

/** Where each edge leaves its node: one place per outgoing edge, in port order. */
function leavingOrder(
  graph: WorkflowGraph,
  drawn: readonly DrawnEdge[],
): ReadonlyMap<string, readonly number[]> {
  const leaving = new Map<string, number[]>();
  // `toSorted` is stable and `drawn` is in edge order: edges of one rank keep that order.
  const ranked = drawn.toSorted((a, b) => portRank(graph, a.edge) - portRank(graph, b.edge));
  for (const { edge, index } of ranked) {
    leaving.set(edge.from, [...(leaving.get(edge.from) ?? []), index]);
  }
  return leaving;
}

/** An edge's path from its place on the side of its node; `lane` is set for a back edge. */
function placeEdge(
  drawn: DrawnEdge,
  leaving: ReadonlyMap<string, readonly number[]>,
  lane: number | undefined,
  frame: Frame,
): PlacedEdge {
  const { edge, index, from, to } = drawn;
  const mine = leaving.get(edge.from) ?? [index];
  const share = (mine.indexOf(index) + 1) / (mine.length + 1);
  const start = edgeStart(from, share, frame.across);
  const label = frame.across
    ? { x: start.x + 6, y: start.y - 5, anchor: "start" as const }
    : { x: start.x + 5, y: start.y + 14, anchor: "start" as const };
  const path =
    lane === undefined
      ? forwardPath(start, to, frame.across)
      : backPath(start, from, to, lane, frame);
  return { index, edge, back: lane !== undefined, d: path.d, label, middle: path.middle };
}

/** Where an edge leaves its node: on the side that faces the next layer, at `share` of its length. */
function edgeStart(from: PlacedNode, share: number, across: boolean): Point {
  return across
    ? { x: from.x + from.w, y: from.y + from.h * share }
    : { x: from.x + from.w * share, y: from.y + from.h };
}

/** A curve into the middle of the target's near side. */
function forwardPath(start: Point, to: PlacedNode, across: boolean): { d: string; middle: Point } {
  const x1 = across ? to.x : to.x + to.w / 2;
  const y1 = across ? to.y + to.h / 2 : to.y;
  return {
    d: cubic(start.x, start.y, x1, y1, across),
    middle: { x: (start.x + x1) / 2, y: (start.y + y1) / 2 },
  };
}

/** Out into the gap after the source's layer, around the outside, back through the gap before the target's. */
function backPath(
  start: Point,
  from: PlacedNode,
  to: PlacedNode,
  lane: number,
  frame: Frame,
): { d: string; middle: Point } {
  const inset = Math.min(10 + lane * 6, frame.layerGap / 2 - 4);
  const outside = frame.nodesEnd + LANE_OFFSET + lane * LANE_STEP;
  const points: Point[] = frame.across
    ? [
        start,
        { x: from.x + from.w + inset, y: start.y },
        { x: from.x + from.w + inset, y: outside },
        { x: to.x - inset, y: outside },
        { x: to.x - inset, y: to.y + to.h * 0.75 },
        { x: to.x, y: to.y + to.h * 0.75 },
      ]
    : [
        start,
        { x: start.x, y: from.y + from.h + inset },
        { x: outside, y: from.y + from.h + inset },
        { x: outside, y: to.y - inset },
        { x: to.x + to.w * 0.75, y: to.y - inset },
        { x: to.x + to.w * 0.75, y: to.y },
      ];
  const middle = frame.across
    ? { x: (points[2].x + points[3].x) / 2, y: outside }
    : { x: outside, y: (points[2].y + points[3].y) / 2 };
  return { d: orthogonal(points), middle };
}

/** The drawing's size: its layers one way, its nodes and the back edges' lanes the other. */
function drawingSize(
  frame: Frame,
  layerCount: number,
  backCount: number,
): { width: number; height: number } {
  const lanesRoom = backCount === 0 ? 0 : LANE_OFFSET + (backCount - 1) * LANE_STEP + 8;
  const layersEnd = PAD + Math.max(0, layerCount * frame.layerStep - frame.layerGap) + PAD;
  const nodesEnd = frame.nodesEnd + lanesRoom + PAD;
  return frame.across
    ? { width: layersEnd, height: nodesEnd }
    : { width: nodesEnd, height: layersEnd };
}
