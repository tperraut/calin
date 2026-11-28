import type { EdgeKind } from "../../gen/EdgeKind";
import type { NodeKind } from "../../gen/NodeKind";
import { COLUMN_STEP, SLOT, type LayoutMode } from "./layout";

/**
 * Routes the edges drawn over the spawn tree (notes, reports, changed files)
 * in lanes, never through a card. Pure: positions and edges in, paths out.
 *
 * In the indented layout every lane lies in one gutter to the right of the
 * tree and the repo band: an edge leaves its card's right side, runs along its
 * lane, and comes back into the other card's right side. In the column layout
 * the lanes lie in the corridors between columns: an edge between neighbouring
 * columns uses the corridor between them, one between cards of one column the
 * corridor to its right, and one that has to cross a column it cannot pass
 * runs along the gap between two rows. Edges between the same two cards share
 * a lane as one bundle, a line each; edges into a repo meet on one trunk and
 * enter it at one point. Lanes are given shortest span first, each to the
 * free lane that crosses the fewest lanes already given, and an edge keeps its
 * lane for as long as it exists (`previous`), until the caller starts over.
 *
 * It runs in phases: edges gather into bundles; each bundle is planned as
 * pieces (what takes a lane), tracks (runs along a gap between rows), ports
 * and a draft per edge; then lanes, then tracks, are given; then the drafts
 * are drawn, once every lane and port is known.
 */

export type RouteBox = {
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
  readonly kind: NodeKind;
};

export type OverlayEdge = {
  readonly id: string;
  readonly from: string;
  readonly to: string;
  readonly kind: EdgeKind | "link";
};

/**
 * A corner of a path. A coordinate with a node (`xn`, `yn`) is an offset from
 * where that node is, so the path follows the node while it moves; one without
 * is where it is in the drawing.
 */
export type RoutePoint = {
  readonly x: number;
  readonly y: number;
  readonly xn?: string;
  readonly yn?: string;
};

/** Where an edge's count sits: a share of its length, or a distance from its start (from its end when negative). */
export type LabelAt = { readonly fraction: number } | { readonly distance: number };

export type OverlayRoute = {
  readonly points: readonly RoutePoint[];
  readonly label: LabelAt;
  /** The edges that travel together: between the same two cards, or into the same repo. */
  readonly bundle: string;
  /** Where the lane is: `gutter`, or `c<n>` for the corridor right of column n. */
  readonly channel: string;
  readonly lane: number;
};

/** The lane each bundle had, by `channel|bundle`, and the track each row crossing had. */
export type LaneMemory = ReadonlyMap<string, number>;

export type Routing = {
  readonly routes: ReadonlyMap<string, OverlayRoute>;
  readonly memory: LaneMemory;
  /** How far right the lanes reach: the drawing is at least this wide. */
  readonly right: number;
};

/** From the widest card to the first lane of the indented layout's gutter. */
const GUTTER_GAP = 14;
const PITCH: Readonly<Record<LayoutMode, number>> = { indented: 7, columns: 6 };
/** Between the lines of one bundle. */
const SUB = 2;
/** Kept free on each side of a corridor between columns. */
const CORRIDOR_MARGIN = 8;
const PORT_STEP = 7;
const PORT_INSET = 7;
/** Two lanes closer than this along the tree are taken as overlapping. */
const PAD = 2;
/** Tracks in the gap between two rows, around its middle. */
const TRACK_SLOTS = [0, -3, 3];
/** Where a repo's body starts under its folder tab, and its middle, from its top. */
const REPO_TOP = 10;
const REPO_MIDDLE = 27;
const AGENT_WIDTH = 176;
/** The height of an agent's card: the gap between two rows is what is left of a `SLOT`. */
const AGENT_ROW = 52;

const kindRank: Readonly<Record<OverlayEdge["kind"], number>> = {
  message: 0,
  report: 1,
  changed: 2,
  dispatched: 3,
  spawned: 4,
  link: 5,
};

type End = { y: number; side: -1 | 1 };

/** A vertical run in one channel: what takes a lane. */
type Piece = {
  key: string;
  channel: string;
  /** What it covers along the tree, cards included. */
  lo: number;
  hi: number;
  /** Where the horizontal runs leave it, and to which side. */
  ends: End[];
  /** Between pieces of the same span: the lower goes first. */
  rank: number;
  lane: number;
};

/** A run along the gap between two rows, from one piece's lane to another's. */
type Track = { key: string; gap: number; from: Piece; to: Piece; slot: number };

type PortRequest = {
  node: string;
  box: RouteBox;
  /** 1: the card's right side; -1: its left side. */
  side: 1 | -1;
  laneX: number;
  /** Where the line goes once it reaches its lane: up (-1), down (1). */
  dir: number;
  key: string;
  dy: number;
};

/** A point of a path, or a reference to a port: the port itself, or the corner level with it. */
type Draft = RoutePoint | { port: PortRequest; corner?: number };

type Card = { id: string; box: RouteBox };

/** An edge with both its cards drawn. */
type Member = { edge: OverlayEdge; from: RouteBox; to: RouteBox };

/** The edges that travel together: between two cards (in id order), or into one repo. */
type Bundle =
  | { kind: "pair"; key: string; a: Card; b: Card; members: Member[] }
  | { kind: "repo"; key: string; repo: Card; members: Member[] };

type Plan = {
  edge: OverlayEdge;
  bundle: string;
  /** The piece whose channel and lane the route reports. */
  piece: Piece;
  /** Its corners, drawn once lanes and tracks are given; it aims its ports on the way. */
  draft: () => Draft[];
  /** A share of the length, or the card whose lines into repos it is spread along. */
  label: LabelAt | { stagger: string };
};

/** What the planning steps read about the drawing. */
type Scene = {
  mode: LayoutMode;
  pitch: number;
  boxes: [string, RouteBox][];
  widest: number;
  columnWidth: Map<number, number>;
};

/** What planning gathers, and what lane allocation fills in for the drafts to read. */
type Board = {
  scene: Scene;
  pieces: Map<string, Piece>;
  tracks: Track[];
  requests: PortRequest[];
  plans: Plan[];
  /** What no lane fits: drawn straight, after the rest. */
  strays: Member[];
  channelLanes: Map<string, number>;
  /** 1: a channel's lanes count from its left; -1: from its right. */
  channelOrder: Map<string, number>;
};

const compare = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);
const middle = (box: RouteBox): number => box.y + box.h / 2;
/** How far the `index`th of `count` lines sits from their middle, in lines. */
const spread = (index: number, count: number): number => index - (count - 1) / 2;
const sub = (index: number, count: number): number => spread(index, count) * SUB;
const columnOf = (box: RouteBox): number => Math.round(box.x / COLUMN_STEP);
const oriented = (path: Draft[], forward: boolean): Draft[] => (forward ? path : path.toReversed());

function pushTo<K, V>(map: Map<K, V[]>, key: K, value: V): void {
  const list = map.get(key);
  if (list) list.push(value);
  else map.set(key, [value]);
}

function overlapping(a: Piece, b: Piece): boolean {
  return a.lo < b.hi + PAD && b.lo < a.hi + PAD;
}

const extents = new WeakMap<Piece, [number, number]>();

/** Where a piece's vertical actually runs, from its first run out to its last. */
function extent(piece: Piece): [number, number] {
  const known = extents.get(piece);
  if (known) return known;
  let lo = Infinity;
  let hi = -Infinity;
  for (const end of piece.ends) {
    lo = Math.min(lo, end.y);
    hi = Math.max(hi, end.y);
  }
  const found: [number, number] = [lo, hi];
  extents.set(piece, found);
  return found;
}

/** How often a horizontal run of `p` on lane `at` passes through the vertical of `q` on lane `other`. */
function runsThrough(p: Piece, at: number, q: Piece, other: number, order: number): number {
  const [qlo, qhi] = extent(q);
  let count = 0;
  for (const end of p.ends) {
    const [near, far] = end.side < 0 ? [other, at] : [at, other];
    if (order * near < order * far && end.y > qlo + 0.5 && end.y < qhi - 0.5) count += 1;
  }
  return count;
}

/**
 * How often `p` on lane `at` and `q` on lane `other` cross: a horizontal run
 * of one through the other's vertical. `order` is 1 when lanes count from the
 * left, -1 when they count from the right.
 */
function crossings(p: Piece, at: number, q: Piece, other: number, order: number): number {
  return runsThrough(p, at, q, other, order) + runsThrough(q, other, p, at, order);
}

function bySpan(a: Piece, b: Piece): number {
  const la = a.hi - a.lo;
  const lb = b.hi - b.lo;
  if (la !== lb) return la < lb ? -1 : 1;
  return a.rank - b.rank || compare(a.key, b.key);
}

/** The pieces of one channel given a lane so far. */
type Placement = { placed: Piece[]; byLane: Map<number, Piece[]>; top: number };

function place(at: Placement, piece: Piece): void {
  at.placed.push(piece);
  pushTo(at.byLane, piece.lane, piece);
  at.top = Math.max(at.top, piece.lane + 1);
}

function fits(at: Placement, piece: Piece, lane: number): boolean {
  return (at.byLane.get(lane) ?? []).every((other) => !overlapping(piece, other));
}

/** The free lane where `piece` crosses the fewest pieces placed, the innermost on a tie, and that count. */
function cheapestLane(at: Placement, piece: Piece, order: number): { lane: number; cost: number } {
  const near = at.placed.filter((other) => overlapping(piece, other));
  const busy = new Set(near.map((other) => other.lane));
  let best = { lane: at.top, cost: Infinity };
  for (let lane = 0; lane <= at.top; lane += 1) {
    if (busy.has(lane)) continue;
    let cost = 0;
    for (const other of near) cost += crossings(piece, lane, other, other.lane, order);
    if (cost < best.cost) best = { lane, cost };
  }
  return best;
}

/**
 * Gives every piece of one channel a lane. A piece that had a lane keeps it
 * while it is free; the others go, shortest first, to the free lane that
 * crosses the fewest pieces already placed, the innermost on a tie. Returns
 * the crossings that leaves.
 */
function allocate(pieces: Piece[], previous: LaneMemory | undefined, order: number): number {
  const at: Placement = { placed: [], byLane: new Map(), top: 0 };
  const fresh: Piece[] = [];
  for (const piece of pieces.toSorted(bySpan)) {
    const was = previous?.get(`${piece.channel}|${piece.key}`);
    if (was !== undefined && fits(at, piece, was)) {
      piece.lane = was;
      place(at, piece);
    } else {
      fresh.push(piece);
    }
  }
  let total = 0;
  for (const piece of fresh) {
    const { lane, cost } = cheapestLane(at, piece, order);
    piece.lane = lane;
    total += cost;
    place(at, piece);
  }
  return total;
}

/** Orders the ports of one side of a card so that the lines leaving it do not cross each other there. */
function placePorts(requests: PortRequest[]): void {
  const { box, side } = requests[0];
  const sideX = side > 0 ? box.x + box.w : box.x;
  const distance = (request: PortRequest): number => Math.abs(request.laneX - sideX);
  const group = (request: PortRequest): number => Math.sign(request.dir);
  // Going up: the nearer lane leaves higher. Going down: the nearer lane leaves lower.
  requests.sort(
    (a, b) =>
      group(a) - group(b) ||
      (group(a) > 0 ? distance(b) - distance(a) : distance(a) - distance(b)) ||
      compare(a.key, b.key),
  );
  const count = requests.length;
  const room = Math.max(0, box.h - 2 * PORT_INSET);
  const step = count > 1 ? Math.min(PORT_STEP, room / (count - 1)) : 0;
  requests.forEach((request, index) => {
    request.dy = box.h / 2 + (index - (count - 1) / 2) * step;
  });
}

// ---- bundles ----

/** The empty bundle an edge travels in, or undefined when no lane fits it: a loop, a line between repos. */
function bundleHead(member: Member): Bundle | undefined {
  const { edge, from, to } = member;
  const intoRepo = to.kind === "repo";
  const outOfRepo = from.kind === "repo";
  if (edge.from === edge.to || (intoRepo && outOfRepo)) return undefined;
  if (intoRepo)
    return { kind: "repo", key: `>${edge.to}`, repo: { id: edge.to, box: to }, members: [] };
  if (outOfRepo)
    return { kind: "repo", key: `>${edge.from}`, repo: { id: edge.from, box: from }, members: [] };
  const start = { id: edge.from, box: from };
  const end = { id: edge.to, box: to };
  const [a, b] = compare(edge.from, edge.to) <= 0 ? [start, end] : [end, start];
  return { kind: "pair", key: `${a.id}|${b.id}`, a, b, members: [] };
}

function byKind(x: Member, y: Member): number {
  return (
    kindRank[x.edge.kind] - kindRank[y.edge.kind] ||
    compare(x.edge.from, y.edge.from) ||
    compare(x.edge.id, y.edge.id)
  );
}

/** The bundles in key order, their edges in kind order, and the edges no lane fits. */
function gatherBundles(
  boxes: ReadonlyMap<string, RouteBox>,
  edges: readonly OverlayEdge[],
): { bundles: Bundle[]; strays: Member[] } {
  const bundles = new Map<string, Bundle>();
  const strays: Member[] = [];
  for (const edge of edges) {
    const from = boxes.get(edge.from);
    const to = boxes.get(edge.to);
    if (!from || !to) continue;
    const member = { edge, from, to };
    const head = bundleHead(member);
    if (!head) {
      strays.push(member);
      continue;
    }
    const bundle = bundles.get(head.key) ?? head;
    bundles.set(bundle.key, bundle);
    bundle.members.push(member);
  }
  for (const bundle of bundles.values()) bundle.members.sort(byKind);
  return { bundles: [...bundles.values()].toSorted((x, y) => compare(x.key, y.key)), strays };
}

// ---- channels ----

function sceneOf(mode: LayoutMode, boxes: ReadonlyMap<string, RouteBox>): Scene {
  const all = [...boxes.entries()];
  let widest = 0;
  const columnWidth = new Map<number, number>();
  for (const [, box] of all) {
    widest = Math.max(widest, box.x + box.w);
    if (box.kind === "repo") continue;
    const at = columnOf(box);
    columnWidth.set(at, Math.max(columnWidth.get(at) ?? 0, box.w));
  }
  return { mode, pitch: PITCH[mode], boxes: all, widest, columnWidth };
}

/** The x of the first lane of a channel, and how many lanes fit before they squeeze. */
function channelBase(scene: Scene, channel: string): { base: number; room: number } {
  if (channel === "gutter") return { base: scene.widest + GUTTER_GAP, room: Infinity };
  const at = Number(channel.slice(1));
  const left = at * COLUMN_STEP + (scene.columnWidth.get(at) ?? AGENT_WIDTH);
  return {
    base: left + CORRIDOR_MARGIN,
    room: Math.max(0, (at + 1) * COLUMN_STEP - left - 2 * CORRIDOR_MARGIN),
  };
}

/** True when nothing but `card` lies level with it between `x0` and `x1`. */
function clearRow(scene: Scene, card: Card, x0: number, x1: number): boolean {
  const { id, box } = card;
  return scene.boxes.every(
    ([other, b]) =>
      other === id ||
      b.x >= x1 ||
      b.x + b.w <= x0 ||
      b.y >= box.y + box.h + PAD ||
      b.y + b.h <= box.y - PAD,
  );
}

/** True when a line from `card` can run level to the first lane of `channel`. */
function clearTo(scene: Scene, card: Card, channel: string): boolean {
  return clearRow(scene, card, card.box.x + card.box.w, channelBase(scene, channel).base);
}

/** The middle of the gap between rows that is next to `box` on the way to `towards`. */
function gapNear(box: RouteBox, towards: number): number {
  return towards < box.y ? box.y - (SLOT - AGENT_ROW) / 2 : box.y + SLOT - (SLOT - AGENT_ROW) / 2;
}

// ---- planning: pieces, tracks, ports and drafts ----

function addPiece(
  board: Board,
  key: string,
  channel: string,
  lo: number,
  hi: number,
  ends: End[],
  rank = 0,
): Piece {
  const made: Piece = { key, channel, lo, hi, ends, rank, lane: 0 };
  board.pieces.set(`${channel}|${key}`, made);
  return made;
}

/** A piece from `box` to the gap between rows at `gap`, its runs at `ends`. */
function addGapPiece(
  board: Board,
  key: string,
  channel: string,
  box: RouteBox,
  gap: number,
  ends: End[],
): Piece {
  return addPiece(
    board,
    key,
    channel,
    Math.min(box.y, gap - 1),
    Math.max(box.y + box.h, gap + 1),
    ends,
  );
}

function addTrack(board: Board, key: string, gap: number, from: Piece, to: Piece): Track {
  const made: Track = { key, gap, from, to, slot: 0 };
  board.tracks.push(made);
  return made;
}

function addPort(board: Board, card: Card, side: 1 | -1, key: string): PortRequest {
  const made: PortRequest = { node: card.id, box: card.box, side, laneX: 0, dir: 0, key, dy: 0 };
  board.requests.push(made);
  return made;
}

/** Points a port at its lane, and up or down towards `towards`. */
function aim(request: PortRequest, laneX: number, towards: number): void {
  request.laneX = laneX;
  request.dir = Math.sign(towards - middle(request.box));
}

/** Where the `member`th of `count` lines of a piece runs, once lanes are given. */
function laneX(board: Board, piece: Piece, member: number, count: number): number {
  const { base, room } = channelBase(board.scene, piece.channel);
  const { pitch } = board.scene;
  const lanes = board.channelLanes.get(piece.channel) ?? 1;
  // A corridor holds what it can; past that its lanes squeeze rather than run into a card.
  const step = lanes > 1 && (lanes - 1) * pitch > room ? room / (lanes - 1) : pitch;
  const offset = sub(member, count) * Math.min(1, step / pitch);
  return (board.channelOrder.get(piece.channel) ?? 1) > 0
    ? base + piece.lane * step + offset
    : base + Math.min(room, (lanes - 1) * step) - piece.lane * step + offset;
}

const fractionLabel = (index: number, count: number): LabelAt => ({
  fraction: 0.5 + spread(index, count) * 0.16,
});

const portSide = (id: string, left: Card, sameSide: boolean): 1 | -1 =>
  id === left.id || sameSide ? 1 : -1;

/** Out of one card, along one lane, into the other: the two cards' sides face the lane. */
function planLane(
  board: Board,
  bundle: Extract<Bundle, { kind: "pair" }>,
  channel: string,
  [left, right]: [Card, Card],
  sameSide: boolean,
): void {
  const lane = addPiece(
    board,
    bundle.key,
    channel,
    Math.min(left.box.y, right.box.y),
    Math.max(left.box.y + left.box.h, right.box.y + right.box.h),
    [
      { y: middle(left.box), side: -1 },
      { y: middle(right.box), side: sameSide ? -1 : 1 },
    ],
  );
  const count = bundle.members.length;
  bundle.members.forEach((member, index) => {
    const { edge } = member;
    const start = addPort(
      board,
      { id: edge.from, box: member.from },
      portSide(edge.from, left, sameSide),
      edge.id,
    );
    const end = addPort(
      board,
      { id: edge.to, box: member.to },
      portSide(edge.to, left, sameSide),
      edge.id,
    );
    const draft = (): Draft[] => {
      const x = laneX(board, lane, index, count);
      aim(start, x, middle(member.to));
      aim(end, x, middle(member.from));
      return [{ port: start }, { port: start, corner: x }, { port: end, corner: x }, { port: end }];
    };
    board.plans.push({
      edge,
      bundle: bundle.key,
      piece: lane,
      draft,
      label: fractionLabel(index, count),
    });
  });
}

/** The two lanes and the track of a line that crosses a column along the gap between two rows. */
type Crossing = { left: Card; right: Card; first: Piece; second: Piece; run: Track };

function crossingDraft(
  board: Board,
  way: Crossing,
  [start, end]: [PortRequest, PortRequest],
  forward: boolean,
  [index, count]: [number, number],
): () => Draft[] {
  return () => {
    const x1 = laneX(board, way.first, index, count);
    const x2 = laneX(board, way.second, index, count);
    const y = way.run.gap + TRACK_SLOTS[way.run.slot] + sub(index, count) * 0.6;
    aim(start, x1, y);
    aim(end, x2, y);
    const yn = way.left.id;
    const top = y - way.left.box.y;
    return oriented(
      [
        { port: start },
        { port: start, corner: x1 },
        { x: x1, y: top, yn },
        { x: x2, y: top, yn },
        { port: end, corner: x2 },
        { port: end },
      ],
      forward,
    );
  };
}

/** Down the corridor right of the left card, along a gap between rows, up the one left of the right card. */
function planCrossing(
  board: Board,
  bundle: Extract<Bundle, { kind: "pair" }>,
  [near, far]: [number, number],
  [left, right]: [Card, Card],
): void {
  const gap = gapNear(left.box, middle(right.box));
  const first = addGapPiece(board, `${bundle.key}@1`, `c${near}`, left.box, gap, [
    { y: middle(left.box), side: -1 },
    { y: gap, side: 1 },
  ]);
  const second = addGapPiece(board, `${bundle.key}@2`, `c${far}`, right.box, gap, [
    { y: gap, side: -1 },
    { y: middle(right.box), side: 1 },
  ]);
  const way: Crossing = {
    left,
    right,
    first,
    second,
    run: addTrack(board, bundle.key, gap, first, second),
  };
  const count = bundle.members.length;
  bundle.members.forEach(({ edge }, index) => {
    const ports: [PortRequest, PortRequest] = [
      addPort(board, left, 1, edge.id),
      addPort(board, right, -1, edge.id),
    ];
    board.plans.push({
      edge,
      bundle: bundle.key,
      piece: second,
      draft: crossingDraft(board, way, ports, edge.from === left.id, [index, count]),
      label: fractionLabel(index, count),
    });
  });
}

function planPair(board: Board, bundle: Extract<Bundle, { kind: "pair" }>): void {
  const colA = columnOf(bundle.a.box);
  const colB = columnOf(bundle.b.box);
  // The one further left, and the other.
  const ends: [Card, Card] = colA <= colB ? [bundle.a, bundle.b] : [bundle.b, bundle.a];
  if (board.scene.mode === "indented") return planLane(board, bundle, "gutter", ends, true);
  if (colA === colB) return planLane(board, bundle, `c${colA}`, ends, true);
  const near = Math.min(colA, colB);
  const far = Math.max(colA, colB) - 1;
  if (far === near || clearTo(board.scene, ends[0], `c${far}`)) {
    return planLane(board, bundle, `c${far}`, ends, false);
  }
  planCrossing(board, bundle, [near, far], ends);
}

/** The agent's end of an edge into or out of `repo`. */
function agentEnd(repo: string, { edge, from, to }: Member): Card {
  return edge.to === repo ? { id: edge.from, box: from } : { id: edge.to, box: to };
}

/** How high the track over the indented repo band runs into `repo`. */
function bandTrackY(scene: Scene, repo: Card): number {
  const row = scene.boxes
    .filter(([, box]) => box.kind === "repo" && box.y === repo.box.y)
    .toSorted(([, a], [, b]) => a.x - b.x);
  const place = Math.max(
    0,
    row.findIndex(([id]) => id === repo.id),
  );
  let above = -Infinity;
  for (const [, box] of scene.boxes) {
    if (box.y + box.h <= repo.box.y) above = Math.max(above, box.y + box.h);
  }
  if (above === -Infinity) above = repo.box.y - 2 * SLOT;
  const room = repo.box.y - above;
  // The leftmost repo's track is the highest, so no track crosses another's way down.
  return above + (room * (place + 1)) / (row.length + 1);
}

/** The band under the tree: reached from above, along the gap over its row. */
function planBand(board: Board, bundle: Extract<Bundle, { kind: "repo" }>): void {
  const { repo } = bundle;
  const trackY = bandTrackY(board.scene, repo);
  const agents = bundle.members.map((member) => agentEnd(repo.id, member));
  // A trunk keeps its lane from the top of the tree whoever joins it; the shortest inside.
  const top = Math.min(...agents.map((agent) => middle(agent.box)));
  const trunk = addPiece(
    board,
    bundle.key,
    "gutter",
    -Infinity,
    trackY + 1,
    [...agents.map((agent): End => ({ y: middle(agent.box), side: -1 })), { y: trackY, side: -1 }],
    trackY - top,
  );
  bundle.members.forEach(({ edge }, index) => {
    const agent = agents[index];
    const start = addPort(board, agent, 1, edge.id);
    const draft = (): Draft[] => {
      const x = laneX(board, trunk, 0, 1);
      aim(start, x, trackY);
      const { id, box } = repo;
      const y = trackY - box.y;
      return oriented(
        [
          { port: start },
          { port: start, corner: x },
          { x, y, yn: id },
          { x: box.w / 2, xn: id, y, yn: id },
          { x: box.w / 2, xn: id, y: REPO_TOP, yn: id },
        ],
        edge.to === id,
      );
    };
    board.plans.push({
      edge,
      bundle: bundle.key,
      piece: trunk,
      draft,
      label: { stagger: agent.id },
    });
  });
}

/** An edge on its way to a repo's trunk: straight across, or first along the gap between rows. */
type Arrival = { member: Member; agent: Card; end: End; via?: { gap: number; lane: Piece } };

/** How an edge reaches the trunk in `channel`, or undefined when its agent is not left of the repo. */
function arrival(
  board: Board,
  bundle: Extract<Bundle, { kind: "repo" }>,
  channel: string,
  member: Member,
): Arrival | undefined {
  const { repo } = bundle;
  const agent = agentEnd(repo.id, member);
  const at = columnOf(agent.box);
  const repoColumn = columnOf(repo.box);
  if (at >= repoColumn) return undefined;
  if (at === repoColumn - 1 || clearTo(board.scene, agent, channel)) {
    return { member, agent, end: { y: middle(agent.box), side: -1 } };
  }
  const gap = gapNear(agent.box, repo.box.y + REPO_MIDDLE);
  const lane = addGapPiece(board, `${bundle.key}@${agent.id}`, `c${at}`, agent.box, gap, [
    { y: middle(agent.box), side: -1 },
    { y: gap, side: 1 },
  ]);
  return { member, agent, end: { y: gap, side: -1 }, via: { gap, lane } };
}

/** The start of a path into the trunk at `x`: along its own lane and gap first when it has to. */
function toTrunk(
  board: Board,
  start: PortRequest,
  agent: Card,
  via: { gap: number; lane: Piece; run: Track } | undefined,
  [x, entryY]: [number, number],
): Draft[] {
  if (!via) {
    aim(start, x, entryY);
    return [{ port: start }, { port: start, corner: x }];
  }
  const x1 = laneX(board, via.lane, 0, 1);
  const y = via.gap + TRACK_SLOTS[via.run.slot];
  aim(start, x1, y);
  const yn = agent.id;
  return [
    { port: start },
    { port: start, corner: x1 },
    { x: x1, y: y - agent.box.y, yn },
    { x, y: y - agent.box.y, yn },
  ];
}

/** Columns: the trunk lies in the corridor left of the repo column, and enters its left side. */
function planCorridor(board: Board, bundle: Extract<Bundle, { kind: "repo" }>): void {
  const { repo } = bundle;
  const channel = `c${columnOf(repo.box) - 1}`;
  const entryY = repo.box.y + REPO_MIDDLE;
  const ends: End[] = [{ y: entryY, side: 1 }];
  const arrivals: Arrival[] = [];
  for (const member of bundle.members) {
    const found = arrival(board, bundle, channel, member);
    if (!found) {
      board.strays.push(member);
      continue;
    }
    ends.push(found.end);
    arrivals.push(found);
  }
  // The corridor by the repos holds their trunks, each a lane of its own whoever joins it.
  const ys = ends.map((end) => end.y);
  const trunk = addPiece(
    board,
    bundle.key,
    channel,
    -Infinity,
    Infinity,
    ends,
    Math.max(...ys) - Math.min(...ys),
  );
  for (const one of arrivals) {
    const { edge } = one.member;
    const via = one.via && {
      ...one.via,
      run: addTrack(board, `${bundle.key}@${one.agent.id}`, one.via.gap, one.via.lane, trunk),
    };
    const start = addPort(board, one.agent, 1, edge.id);
    const draft = (): Draft[] => {
      const x = laneX(board, trunk, 0, 1);
      const id = repo.id;
      return oriented(
        [
          ...toTrunk(board, start, one.agent, via, [x, entryY]),
          { x, y: REPO_MIDDLE, yn: id },
          { x: 0, xn: id, y: REPO_MIDDLE, yn: id },
        ],
        edge.to === id,
      );
    };
    board.plans.push({
      edge,
      bundle: bundle.key,
      piece: trunk,
      draft,
      label: { stagger: one.agent.id },
    });
  }
}

function planBundle(board: Board, bundle: Bundle): void {
  if (bundle.kind === "pair") planPair(board, bundle);
  else if (board.scene.mode === "indented") planBand(board, bundle);
  else planCorridor(board, bundle);
}

// ---- lanes ----

/** Gives a channel's pieces their lanes: a corridor's count from whichever side crosses less, and keep to it. */
function laneOrder(channel: string, list: Piece[], previous: LaneMemory | undefined): number {
  const order = channel === "gutter" ? 1 : (previous?.get(`order|${channel}`) ?? 0);
  if (order !== 0) {
    allocate(list, previous, order);
    return order;
  }
  const fromLeft = allocate(list, undefined, 1);
  const lanes = list.map((one) => one.lane);
  const fromRight = allocate(list, undefined, -1);
  if (fromRight < fromLeft) return -1;
  list.forEach((one, index) => (one.lane = lanes[index]));
  return 1;
}

/** Gives every piece its lane, records them, and returns how far right the lanes reach. */
function allocateChannels(
  board: Board,
  previous: LaneMemory | undefined,
  memory: Map<string, number>,
): number {
  const byChannel = new Map<string, Piece[]>();
  for (const one of board.pieces.values()) pushTo(byChannel, one.channel, one);
  let right = board.scene.widest;
  for (const [channel, list] of [...byChannel].toSorted(([a], [b]) => compare(a, b))) {
    const order = laneOrder(channel, list, previous);
    board.channelOrder.set(channel, order);
    memory.set(`order|${channel}`, order);
    let lanes = 0;
    for (const one of list) {
      memory.set(`${channel}|${one.key}`, one.lane);
      lanes = Math.max(lanes, one.lane + 1);
    }
    board.channelLanes.set(channel, lanes);
    const { base } = channelBase(board.scene, channel);
    right = Math.max(right, base + (lanes - 1) * board.scene.pitch + SUB * 2);
  }
  return right;
}

// ---- tracks along the gaps between rows ----

/** A track given its slot, and where it runs. */
type Laid = { gap: number; slot: number; lo: number; hi: number };

function slotFree(taken: Laid[], gap: number, [lo, hi]: [number, number], slot: number): boolean {
  return taken.every(
    (other) => other.gap !== gap || other.slot !== slot || other.hi < lo || other.lo > hi,
  );
}

/** Gives each track a slot in its gap: the one it had while free, else the first free. */
function assignTracks(
  board: Board,
  previous: LaneMemory | undefined,
  memory: Map<string, number>,
): void {
  const taken: Laid[] = [];
  for (const run of board.tracks.toSorted((a, b) => compare(a.key, b.key))) {
    const xs = [laneX(board, run.from, 0, 1), laneX(board, run.to, 0, 1)];
    const span: [number, number] = [Math.min(...xs), Math.max(...xs)];
    const was = previous?.get(`track|${run.key}`);
    run.slot =
      was !== undefined && slotFree(taken, run.gap, span, was)
        ? was
        : Math.max(
            0,
            TRACK_SLOTS.findIndex((_, index) => slotFree(taken, run.gap, span, index)),
          );
    memory.set(`track|${run.key}`, run.slot);
    taken.push({ gap: run.gap, slot: run.slot, lo: span[0], hi: span[1] });
  }
}

// ---- ports, then the paths ----

function pointOf(one: Draft): RoutePoint {
  if (!("port" in one)) return one;
  const { node, dy, side, box } = one.port;
  if (one.corner !== undefined) return { x: one.corner, y: dy, yn: node };
  return { x: side > 0 ? box.w : 0, xn: node, y: dy, yn: node };
}

/** Counts on the lines out of one card into repos are spread along them, not stacked. */
function labelOf(plan: Plan, outOfCard: Map<string, number>): LabelAt {
  if (!("stagger" in plan.label)) return plan.label;
  const agent = plan.label.stagger;
  const seen = outOfCard.get(agent) ?? 0;
  outOfCard.set(agent, seen + 1);
  const distance = 22 + seen * 24;
  // Near the agent's end, whichever way the edge runs.
  return plan.edge.from === agent ? { distance } : { distance: -distance };
}

function drawPlans(board: Board, routes: Map<string, OverlayRoute>): void {
  const drafts = board.plans.map((plan) => ({ plan, draft: plan.draft() }));
  const bySide = new Map<string, PortRequest[]>();
  for (const request of board.requests) pushTo(bySide, `${request.node}|${request.side}`, request);
  for (const list of bySide.values()) placePorts(list);
  const outOfCard = new Map<string, number>();
  for (const { plan, draft } of drafts) {
    routes.set(plan.edge.id, {
      points: draft.map(pointOf),
      label: labelOf(plan, outOfCard),
      bundle: plan.bundle,
      channel: plan.piece.channel,
      lane: plan.piece.lane,
    });
  }
}

/** What no lane fits: a loop on a card, a line between two repos. */
function strayRoute({ edge, from, to }: Member): OverlayRoute {
  const loop = (dy: number, dx: number): RoutePoint => ({
    x: from.w + dx,
    xn: edge.from,
    y: from.h / 2 + dy,
    yn: edge.from,
  });
  const points: RoutePoint[] =
    edge.from === edge.to
      ? [loop(-6, 0), loop(-6, 14), loop(6, 14), loop(6, 0)]
      : [
          { x: from.w / 2, xn: edge.from, y: from.h / 2, yn: edge.from },
          { x: to.w / 2, xn: edge.to, y: to.h / 2, yn: edge.to },
        ];
  return { points, label: { fraction: 0.5 }, bundle: edge.id, channel: "", lane: 0 };
}

export function routeOverlay(
  mode: LayoutMode,
  boxes: ReadonlyMap<string, RouteBox>,
  edges: readonly OverlayEdge[],
  previous?: LaneMemory,
): Routing {
  const { bundles, strays } = gatherBundles(boxes, edges);
  const board: Board = {
    scene: sceneOf(mode, boxes),
    pieces: new Map(),
    tracks: [],
    requests: [],
    plans: [],
    strays,
    channelLanes: new Map(),
    channelOrder: new Map(),
  };
  for (const bundle of bundles) planBundle(board, bundle);
  const memory = new Map<string, number>();
  const right = allocateChannels(board, previous, memory);
  assignTracks(board, previous, memory);
  const routes = new Map<string, OverlayRoute>();
  drawPlans(board, routes);
  for (const stray of board.strays) routes.set(stray.edge.id, strayRoute(stray));
  return { routes, memory, right };
}

/** Every corner of a route, where it is in the drawing. */
export function resolvePoints(
  points: readonly RoutePoint[],
  at: (id: string) => { x: number; y: number } | undefined,
): { x: number; y: number }[] {
  return points.map((point) => {
    const xs = point.xn === undefined ? undefined : at(point.xn);
    const ys = point.yn === undefined ? undefined : at(point.yn);
    return { x: point.x + (xs?.x ?? 0), y: point.y + (ys?.y ?? 0) };
  });
}
