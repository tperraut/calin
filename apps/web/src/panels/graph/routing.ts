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

const kindRank: Readonly<Record<string, number>> = {
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

type PortRequest = {
  node: string;
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

type Bundle = {
  key: string;
  /** The repo it ends in, for edges into a repo. */
  repo: string | undefined;
  /** The two cards, in id order; for a repo bundle, the agents. */
  a: string;
  b: string;
  members: OverlayEdge[];
};

const compare = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);
const middle = (box: RouteBox): number => box.y + box.h / 2;
const sub = (index: number, count: number): number => (index - (count - 1) / 2) * SUB;

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

/**
 * How often `p` on lane `at` and `q` on lane `other` cross: a horizontal run
 * of one through the other's vertical. `order` is 1 when lanes count from the
 * left, -1 when they count from the right.
 */
function crossings(p: Piece, at: number, q: Piece, other: number, order: number): number {
  let count = 0;
  const [plo, phi] = extent(p);
  const [qlo, qhi] = extent(q);
  const left = (a: number, b: number): boolean => order * a < order * b;
  for (const end of p.ends) {
    const between = end.side < 0 ? left(other, at) : left(at, other);
    if (between && end.y > qlo + 0.5 && end.y < qhi - 0.5) count += 1;
  }
  for (const end of q.ends) {
    const between = end.side < 0 ? left(at, other) : left(other, at);
    if (between && end.y > plo + 0.5 && end.y < phi - 0.5) count += 1;
  }
  return count;
}

function bySpan(a: Piece, b: Piece): number {
  const la = a.hi - a.lo;
  const lb = b.hi - b.lo;
  if (la !== lb) return la < lb ? -1 : 1;
  return a.rank - b.rank || compare(a.key, b.key);
}

/**
 * Gives every piece of one channel a lane. A piece that had a lane keeps it
 * while it is free; the others go, shortest first, to the free lane that
 * crosses the fewest pieces already placed, the innermost on a tie. Returns
 * the crossings that leaves.
 */
function allocate(pieces: Piece[], previous: LaneMemory | undefined, order: number): number {
  const sorted = pieces.toSorted(bySpan);
  const placed: Piece[] = [];
  const byLane = new Map<number, Piece[]>();
  let top = 0;
  const fits = (piece: Piece, lane: number): boolean =>
    (byLane.get(lane) ?? []).every((other) => !overlapping(piece, other));
  const place = (piece: Piece): void => {
    placed.push(piece);
    const list = byLane.get(piece.lane);
    if (list) list.push(piece);
    else byLane.set(piece.lane, [piece]);
    top = Math.max(top, piece.lane + 1);
  };
  const fresh: Piece[] = [];
  for (const piece of sorted) {
    const was = previous?.get(`${piece.channel}|${piece.key}`);
    if (was !== undefined && fits(piece, was)) {
      piece.lane = was;
      place(piece);
    } else {
      fresh.push(piece);
    }
  }
  let total = 0;
  for (const piece of fresh) {
    const near = placed.filter((other) => overlapping(piece, other));
    const busy = new Set(near.map((other) => other.lane));
    let best = top;
    let bestCost = Infinity;
    for (let lane = 0; lane <= top; lane += 1) {
      if (busy.has(lane)) continue;
      let cost = 0;
      for (const other of near) cost += crossings(piece, lane, other, other.lane, order);
      if (cost < bestCost) {
        best = lane;
        bestCost = cost;
      }
    }
    piece.lane = best;
    total += bestCost;
    place(piece);
  }
  return total;
}

/** Orders the ports of one side of a card so that the lines leaving it do not cross each other there. */
function placePorts(requests: PortRequest[], box: RouteBox): void {
  const sideX = requests[0].side > 0 ? box.x + box.w : box.x;
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

export function routeOverlay(
  mode: LayoutMode,
  boxes: ReadonlyMap<string, RouteBox>,
  edges: readonly OverlayEdge[],
  previous?: LaneMemory,
): Routing {
  const routes = new Map<string, OverlayRoute>();
  const memory = new Map<string, number>();
  const pitch = PITCH[mode];

  // ---- bundles ----
  const bundles = new Map<string, Bundle>();
  const strays: OverlayEdge[] = [];
  for (const edge of edges) {
    const from = boxes.get(edge.from);
    const to = boxes.get(edge.to);
    if (!from || !to) continue;
    const intoRepo = to.kind === "repo";
    const outOfRepo = from.kind === "repo";
    if (edge.from === edge.to || (intoRepo && outOfRepo)) {
      strays.push(edge);
      continue;
    }
    let key: string;
    let bundle: Omit<Bundle, "members">;
    if (intoRepo || outOfRepo) {
      const repo = intoRepo ? edge.to : edge.from;
      key = `>${repo}`;
      bundle = { key, repo, a: "", b: "" };
    } else {
      const [a, b] = compare(edge.from, edge.to) <= 0 ? [edge.from, edge.to] : [edge.to, edge.from];
      key = `${a}|${b}`;
      bundle = { key, repo: undefined, a, b };
    }
    const existing = bundles.get(key);
    if (existing) existing.members.push(edge);
    else bundles.set(key, { ...bundle, members: [edge] });
  }
  for (const bundle of bundles.values()) {
    bundle.members.sort(
      (x, y) =>
        (kindRank[x.kind] ?? 9) - (kindRank[y.kind] ?? 9) ||
        compare(x.from, y.from) ||
        compare(x.id, y.id),
    );
  }
  const ordered = [...bundles.values()].toSorted((x, y) => compare(x.key, y.key));

  // ---- channels ----
  const all = [...boxes.entries()];
  let widest = 0;
  for (const [, box] of all) widest = Math.max(widest, box.x + box.w);
  const column = (box: RouteBox): number => Math.round(box.x / COLUMN_STEP);
  const columnWidth = new Map<number, number>();
  for (const [, box] of all) {
    if (box.kind === "repo") continue;
    const at = column(box);
    columnWidth.set(at, Math.max(columnWidth.get(at) ?? 0, box.w));
  }
  /** The x of the first lane of a channel, and how many lanes fit before they squeeze. */
  const channelBase = (channel: string): { base: number; room: number } => {
    if (channel === "gutter") return { base: widest + GUTTER_GAP, room: Infinity };
    const at = Number(channel.slice(1));
    const left = at * COLUMN_STEP + (columnWidth.get(at) ?? AGENT_WIDTH);
    return {
      base: left + CORRIDOR_MARGIN,
      room: Math.max(0, (at + 1) * COLUMN_STEP - left - 2 * CORRIDOR_MARGIN),
    };
  };

  /** True when nothing but `box` lies level with it between `x0` and `x1`. */
  const clearRow = (id: string, box: RouteBox, x0: number, x1: number): boolean =>
    all.every(
      ([other, b]) =>
        other === id ||
        b.x >= x1 ||
        b.x + b.w <= x0 ||
        b.y >= box.y + box.h + PAD ||
        b.y + b.h <= box.y - PAD,
    );
  /** The middle of the gap between rows that is next to `box` on the way to `towards`. */
  const gapNear = (box: RouteBox, towards: number): number =>
    towards < box.y ? box.y - (SLOT - AGENT_ROW) / 2 : box.y + SLOT - (SLOT - AGENT_ROW) / 2;

  // ---- pieces, and how to draw each edge once lanes are known ----
  const pieces = new Map<string, Piece>();
  const piece = (
    key: string,
    channel: string,
    lo: number,
    hi: number,
    ends: End[],
    rank = 0,
  ): ((member: number, count: number) => number) => {
    const made: Piece = { key, channel, lo, hi, ends, rank, lane: 0 };
    pieces.set(`${channel}|${key}`, made);
    return (member, count) => {
      const { base, room } = channelBase(channel);
      const lanes = channelLanes.get(channel) ?? 1;
      // A corridor holds what it can; past that its lanes squeeze rather than run into a card.
      const step = lanes > 1 && (lanes - 1) * pitch > room ? room / (lanes - 1) : pitch;
      const offset = sub(member, count) * Math.min(1, step / pitch);
      return (channelOrder.get(channel) ?? 1) > 0
        ? base + made.lane * step + offset
        : base + Math.min(room, (lanes - 1) * step) - made.lane * step + offset;
    };
  };
  const channelLanes = new Map<string, number>();
  /** 1: a channel's lanes count from its left; -1: from its right. */
  const channelOrder = new Map<string, number>();
  type Track = { gap: number; x0: () => number; x1: () => number; key: string; slot: number };
  const tracks: Track[] = [];
  const track = (key: string, gap: number, x0: () => number, x1: () => number): Track => {
    const made: Track = { gap, x0, x1, key, slot: 0 };
    tracks.push(made);
    return made;
  };
  const requests: PortRequest[] = [];
  const port = (node: string, side: 1 | -1, key: string): PortRequest => {
    const made: PortRequest = { node, side, laneX: 0, dir: 0, key, dy: 0 };
    requests.push(made);
    return made;
  };
  type Plan = {
    edge: OverlayEdge;
    bundle: string;
    channel: string;
    lane: () => number;
    draft: () => Draft[];
    /** A share of the length, or the card whose lines into repos it is spread along. */
    label: LabelAt | { stagger: string };
  };
  const plans: Plan[] = [];

  for (const bundle of ordered) {
    const count = bundle.members.length;
    if (bundle.repo === undefined) {
      const boxA = boxes.get(bundle.a);
      const boxB = boxes.get(bundle.b);
      if (!boxA || !boxB) continue;
      const span = (...list: RouteBox[]): [number, number] => [
        Math.min(...list.map((box) => box.y)),
        Math.max(...list.map((box) => box.y + box.h)),
      ];
      const [lo, hi] = span(boxA, boxB);
      const colA = column(boxA);
      const colB = column(boxB);
      // The one further left, and the other.
      const [left, right] = colA <= colB ? [bundle.a, bundle.b] : [bundle.b, bundle.a];
      const boxL = boxes.get(left) as RouteBox;
      const boxR = boxes.get(right) as RouteBox;
      const sameSide = mode === "indented" || colA === colB;
      let shape: "side" | "across" | "track";
      let channel: string;
      if (mode === "indented") {
        shape = "side";
        channel = "gutter";
      } else if (colA === colB) {
        shape = "side";
        channel = `c${colA}`;
      } else {
        const far = Math.max(colA, colB) - 1;
        const direct =
          far === Math.min(colA, colB) ||
          clearRow(left, boxL, boxL.x + boxL.w, channelBase(`c${far}`).base);
        shape = direct ? "across" : "track";
        channel = `c${far}`;
      }
      if (shape !== "track") {
        const lane = piece(bundle.key, channel, lo, hi, [
          { y: middle(boxL), side: -1 },
          { y: middle(boxR), side: sameSide ? -1 : 1 },
        ]);
        bundle.members.forEach((edge, index) => {
          const start = port(edge.from, edge.from === left || sameSide ? 1 : -1, edge.id);
          const end = port(edge.to, edge.to === left || sameSide ? 1 : -1, edge.id);
          const other = (id: string): RouteBox =>
            boxes.get(id === edge.from ? edge.to : edge.from) as RouteBox;
          const at = (): number => lane(index, count);
          plans.push({
            edge,
            bundle: bundle.key,
            channel,
            lane: () => pieces.get(`${channel}|${bundle.key}`)?.lane ?? 0,
            draft: () => {
              const x = at();
              for (const [request, id] of [
                [start, edge.from],
                [end, edge.to],
              ] as const) {
                request.laneX = x;
                request.dir = Math.sign(middle(other(id)) - middle(boxes.get(id) as RouteBox));
              }
              return [
                { port: start },
                { port: start, corner: x },
                { port: end, corner: x },
                { port: end },
              ];
            },
            label: { fraction: 0.5 + (index - (count - 1) / 2) * 0.16 },
          });
        });
      } else {
        const near = Math.min(colA, colB);
        const far = Math.max(colA, colB) - 1;
        const gap = gapNear(boxL, middle(boxR));
        const first = piece(
          `${bundle.key}@1`,
          `c${near}`,
          Math.min(boxL.y, gap - 1),
          Math.max(boxL.y + boxL.h, gap + 1),
          [
            { y: middle(boxL), side: -1 },
            { y: gap, side: 1 },
          ],
        );
        const second = piece(
          `${bundle.key}@2`,
          `c${far}`,
          Math.min(boxR.y, gap - 1),
          Math.max(boxR.y + boxR.h, gap + 1),
          [
            { y: gap, side: -1 },
            { y: middle(boxR), side: 1 },
          ],
        );
        const run = track(
          `${bundle.key}`,
          gap,
          () => first(0, 1),
          () => second(0, 1),
        );
        bundle.members.forEach((edge, index) => {
          const forward = edge.from === left;
          const startL = port(left, 1, edge.id);
          const endR = port(right, -1, edge.id);
          plans.push({
            edge,
            bundle: bundle.key,
            channel: `c${far}`,
            lane: () => pieces.get(`c${far}|${bundle.key}@2`)?.lane ?? 0,
            draft: () => {
              const x1 = first(index, count);
              const x2 = second(index, count);
              const y = gap + TRACK_SLOTS[run.slot] + sub(index, count) * 0.6;
              startL.laneX = x1;
              startL.dir = Math.sign(y - middle(boxL));
              endR.laneX = x2;
              endR.dir = Math.sign(y - middle(boxR));
              const path: Draft[] = [
                { port: startL },
                { port: startL, corner: x1 },
                { x: x1, y: y - boxL.y, yn: left },
                { x: x2, y: y - boxL.y, yn: left },
                { port: endR, corner: x2 },
                { port: endR },
              ];
              return forward ? path : path.toReversed();
            },
            label: { fraction: 0.5 + (index - (count - 1) / 2) * 0.16 },
          });
        });
      }
      continue;
    }

    // ---- into a repo: one trunk, one way in ----
    const repoId = bundle.repo;
    const repo = boxes.get(repoId);
    if (!repo) continue;
    const agentOf = (edge: OverlayEdge): string => (edge.to === repoId ? edge.from : edge.to);
    if (mode === "indented") {
      // The band under the tree: reached from above, along the gap over its row.
      const row = all
        .filter(([, box]) => box.kind === "repo" && box.y === repo.y)
        .toSorted(([, a], [, b]) => a.x - b.x);
      const place = Math.max(
        0,
        row.findIndex(([id]) => id === repoId),
      );
      let above = -Infinity;
      for (const [, box] of all) {
        if (box.y + box.h <= repo.y) above = Math.max(above, box.y + box.h);
      }
      if (above === -Infinity) above = repo.y - 2 * SLOT;
      const room = repo.y - above;
      // The leftmost repo's track is the highest, so no track crosses another's way down.
      const trackY = above + (room * (place + 1)) / (row.length + 1);
      const sources = bundle.members
        .map((edge) => boxes.get(agentOf(edge)))
        .filter((box) => box !== undefined);
      // A trunk keeps its lane from the top of the tree whoever joins it; the shortest inside.
      const top = Math.min(...sources.map(middle));
      const trunk = piece(
        bundle.key,
        "gutter",
        -Infinity,
        trackY + 1,
        [...sources.map((box): End => ({ y: middle(box), side: -1 })), { y: trackY, side: -1 }],
        trackY - top,
      );
      bundle.members.forEach((edge) => {
        const agent = agentOf(edge);
        const box = boxes.get(agent) as RouteBox;
        const start = port(agent, 1, edge.id);
        plans.push({
          edge,
          bundle: bundle.key,
          channel: "gutter",
          lane: () => pieces.get(`gutter|${bundle.key}`)?.lane ?? 0,
          draft: () => {
            const x = trunk(0, 1);
            start.laneX = x;
            start.dir = Math.sign(trackY - middle(box));
            const path: Draft[] = [
              { port: start },
              { port: start, corner: x },
              { x, y: trackY - repo.y, yn: repoId },
              { x: repo.w / 2, xn: repoId, y: trackY - repo.y, yn: repoId },
              { x: repo.w / 2, xn: repoId, y: REPO_TOP, yn: repoId },
            ];
            return edge.to === repoId ? path : path.toReversed();
          },
          label: { stagger: agent },
        });
      });
      continue;
    }
    // Columns: the trunk lies in the corridor left of the repo column, and enters its left side.
    const repoColumn = column(repo);
    const trunkChannel = `c${repoColumn - 1}`;
    const ends: End[] = [{ y: repo.y + REPO_MIDDLE, side: 1 }];
    type Arrival = {
      edge: OverlayEdge;
      agent: string;
      box: RouteBox;
      via?: { gap: number; lane: (m: number, c: number) => number; run: Track };
    };
    const arrivals: Arrival[] = [];
    for (const edge of bundle.members) {
      const agent = agentOf(edge);
      const box = boxes.get(agent);
      if (!box) continue;
      const at = column(box);
      if (at >= repoColumn) {
        strays.push(edge);
        continue;
      }
      const direct =
        at === repoColumn - 1 ||
        clearRow(agent, box, box.x + box.w, channelBase(trunkChannel).base);
      if (direct) {
        ends.push({ y: middle(box), side: -1 });
        arrivals.push({ edge, agent, box });
        continue;
      }
      const gap = gapNear(box, repo.y + REPO_MIDDLE);
      const lane = piece(
        `${bundle.key}@${agent}`,
        `c${at}`,
        Math.min(box.y, gap - 1),
        Math.max(box.y + box.h, gap + 1),
        [
          { y: middle(box), side: -1 },
          { y: gap, side: 1 },
        ],
      );
      ends.push({ y: gap, side: -1 });
      arrivals.push({
        edge,
        agent,
        box,
        via: {
          gap,
          lane,
          run: track(
            `${bundle.key}@${agent}`,
            gap,
            () => lane(0, 1),
            () => trunk(0, 1),
          ),
        },
      });
    }
    // The corridor by the repos holds their trunks, each a lane of its own whoever joins it.
    const ys = ends.map((end) => end.y);
    const trunk = piece(
      bundle.key,
      trunkChannel,
      -Infinity,
      Infinity,
      ends,
      Math.max(...ys) - Math.min(...ys),
    );
    for (const { edge, agent, box, via } of arrivals) {
      const start = port(agent, 1, edge.id);
      plans.push({
        edge,
        bundle: bundle.key,
        channel: trunkChannel,
        lane: () => pieces.get(`${trunkChannel}|${bundle.key}`)?.lane ?? 0,
        draft: () => {
          const x = trunk(0, 1);
          const entry: Draft[] = [
            { x, y: REPO_MIDDLE, yn: repoId },
            { x: 0, xn: repoId, y: REPO_MIDDLE, yn: repoId },
          ];
          let path: Draft[];
          if (via) {
            const x1 = via.lane(0, 1);
            const y = via.gap + TRACK_SLOTS[via.run.slot];
            start.laneX = x1;
            start.dir = Math.sign(y - middle(box));
            path = [
              { port: start },
              { port: start, corner: x1 },
              { x: x1, y: y - box.y, yn: agent },
              { x, y: y - box.y, yn: agent },
              ...entry,
            ];
          } else {
            start.laneX = x;
            start.dir = Math.sign(repo.y + REPO_MIDDLE - middle(box));
            path = [{ port: start }, { port: start, corner: x }, ...entry];
          }
          return edge.to === repoId ? path : path.toReversed();
        },
        label: { stagger: agent },
      });
    }
  }

  // ---- lanes ----
  const byChannel = new Map<string, Piece[]>();
  for (const one of pieces.values()) {
    const list = byChannel.get(one.channel);
    if (list) list.push(one);
    else byChannel.set(one.channel, [one]);
  }
  let right = widest;
  for (const [channel, list] of [...byChannel].toSorted(([a], [b]) => compare(a, b))) {
    // A corridor's lanes count from whichever side crosses less, and keep to it.
    const kept = previous?.get(`order|${channel}`);
    let order = channel === "gutter" ? 1 : (kept ?? 0);
    if (order === 0) {
      const fromLeft = allocate(list, undefined, 1);
      const lanes = list.map((one) => one.lane);
      const fromRight = allocate(list, undefined, -1);
      order = fromRight < fromLeft ? -1 : 1;
      if (order > 0) list.forEach((one, index) => (one.lane = lanes[index]));
    } else {
      allocate(list, previous, order);
    }
    channelOrder.set(channel, order);
    memory.set(`order|${channel}`, order);
    let lanes = 0;
    for (const one of list) {
      memory.set(`${channel}|${one.key}`, one.lane);
      lanes = Math.max(lanes, one.lane + 1);
    }
    channelLanes.set(channel, lanes);
    const { base } = channelBase(channel);
    right = Math.max(right, base + (lanes - 1) * pitch + SUB * 2);
  }

  // ---- tracks along the gaps between rows ----
  const taken: Track[] = [];
  for (const run of tracks.toSorted((a, b) => compare(a.key, b.key))) {
    const x0 = Math.min(run.x0(), run.x1());
    const x1 = Math.max(run.x0(), run.x1());
    const free = (slot: number): boolean =>
      taken.every(
        (other) =>
          other.gap !== run.gap ||
          other.slot !== slot ||
          Math.max(other.x0(), other.x1()) < x0 ||
          Math.min(other.x0(), other.x1()) > x1,
      );
    const was = previous?.get(`track|${run.key}`);
    const slot =
      was !== undefined && free(was)
        ? was
        : Math.max(
            0,
            TRACK_SLOTS.findIndex((_, index) => free(index)),
          );
    run.slot = slot;
    memory.set(`track|${run.key}`, slot);
    taken.push(run);
  }

  // ---- ports, then the paths ----
  const drafts = plans.map((plan) => ({ plan, draft: plan.draft() }));
  const bySide = new Map<string, PortRequest[]>();
  for (const request of requests) {
    const key = `${request.node}|${request.side}`;
    const list = bySide.get(key);
    if (list) list.push(request);
    else bySide.set(key, [request]);
  }
  for (const [key, list] of bySide) {
    const box = boxes.get(key.slice(0, key.lastIndexOf("|")));
    if (box) placePorts(list, box);
  }
  // Counts on the lines out of one card into repos are spread along them, not stacked.
  const outOfCard = new Map<string, number>();
  for (const { plan, draft } of drafts) {
    const points = draft.map((one): RoutePoint => {
      if (!("port" in one)) return one;
      const box = boxes.get(one.port.node) as RouteBox;
      const y = one.port.dy;
      if (one.corner !== undefined) return { x: one.corner, y, yn: one.port.node };
      return { x: one.port.side > 0 ? box.w : 0, xn: one.port.node, y, yn: one.port.node };
    });
    let label: LabelAt;
    if ("stagger" in plan.label) {
      const seen = outOfCard.get(plan.label.stagger) ?? 0;
      outOfCard.set(plan.label.stagger, seen + 1);
      const distance = 22 + seen * 24;
      // Near the agent's end, whichever way the edge runs.
      label = plan.edge.from === plan.label.stagger ? { distance } : { distance: -distance };
    } else {
      label = plan.label;
    }
    routes.set(plan.edge.id, {
      points,
      label,
      bundle: plan.bundle,
      channel: plan.channel,
      lane: plan.lane(),
    });
  }

  // ---- what no lane fits: a loop on a card, a line between two repos ----
  for (const edge of strays) {
    const from = boxes.get(edge.from);
    const to = boxes.get(edge.to);
    if (!from || !to) continue;
    const points: RoutePoint[] =
      edge.from === edge.to
        ? [
            { x: from.w, xn: edge.from, y: from.h / 2 - 6, yn: edge.from },
            { x: from.w + 14, xn: edge.from, y: from.h / 2 - 6, yn: edge.from },
            { x: from.w + 14, xn: edge.from, y: from.h / 2 + 6, yn: edge.from },
            { x: from.w, xn: edge.from, y: from.h / 2 + 6, yn: edge.from },
          ]
        : [
            { x: from.w / 2, xn: edge.from, y: from.h / 2, yn: edge.from },
            { x: to.w / 2, xn: edge.to, y: to.h / 2, yn: edge.to },
          ];
    routes.set(edge.id, {
      points,
      label: { fraction: 0.5 },
      bundle: edge.id,
      channel: "",
      lane: 0,
    });
  }
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
