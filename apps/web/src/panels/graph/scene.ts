import {
  fitView,
  polyLength,
  polyPoint,
  routePath,
  routePoint,
  translate,
  treeRoute,
  type Box,
  type Route,
} from "./geometry";
import type { LayoutMode, Point, Size } from "./layout";
import { resolvePoints, type LabelAt, type RoutePoint } from "./routing";

/** A node as the scene needs it: where it goes and how big it is. */
export type SceneNodeInput = {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  /** The node a newcomer flies out of (its parent in the tree), when it has one. */
  origin?: string;
};

export type SceneEdgeInput = {
  id: string;
  from: string;
  to: string;
  route: "tree" | "curve";
  /** For a curve: its corners, from `routeOverlay`. Without them it is drawn straight. */
  points?: readonly RoutePoint[];
  /** For a curve: where its count sits. */
  label?: LabelAt;
};

export type SceneLayout = {
  mode: LayoutMode;
  nodes: readonly SceneNodeInput[];
  edges: readonly SceneEdgeInput[];
  width: number;
  height: number;
  /** The width the first view makes readable, when it is less than `width` (a gutter may run off to the right). */
  homeWidth?: number;
};

export type ViewTransform = { x: number; y: number; k: number };

type SceneNode = SceneNodeInput & {
  /** Where the tween started and where it ends. */
  fx: number;
  fy: number;
  tx: number;
  ty: number;
  moving: boolean;
  /** False while it is far outside the viewport and taken out of the drawing. */
  shown: boolean;
  el?: SVGGElement;
};

/** The line itself, the wider invisible line that takes the taps, and the count. */
type EdgeElements = { path?: SVGPathElement; hit?: SVGPathElement; label?: SVGGElement };
export type EdgePart = keyof EdgeElements;

type SceneEdge = SceneEdgeInput & {
  current?: Route;
  /** For a curve on its way to new lanes: where its corners were when the tween started. */
  start?: readonly Point[];
  /** The path data last written: an unchanged path is not written again. */
  written?: string;
  moving: boolean;
  shown: boolean;
  els?: EdgeElements;
};

type Pulse = { id: string; edge: string; start: number; el?: SVGCircleElement };

export type SceneOptions = {
  /** Read at each change: no tween and no pulse while it is true. */
  reducedMotion: () => boolean;
  /** The pulses to draw changed: Solid creates and removes their elements. */
  onPulses: (ids: readonly string[]) => void;
  /** Told after every change of pan or zoom. */
  onView?: (view: ViewTransform) => void;
  requestFrame?: (run: (time: number) => void) => number;
  cancelFrame?: (handle: number) => void;
  now?: () => number;
};

export const TWEEN_MS = 300;
export const PULSE_MS = 900;
export const MIN_ZOOM = 0.1;
export const MAX_ZOOM = 2.5;
/** Below this zoom the cards drop their text: it could not be read anyway. */
export const LOD_BELOW = 0.45;
const HOME_MIN_ZOOM = 0.6;
/** The smallest zoom at which cards still show their text (see `LOD_BELOW`). */
const LEGIBLE_ZOOM = 0.5;
const PADDING = 16;
/** The drawing starts under the zoom buttons that float over its top edge. */
const HOME_TOP = 60;

const ease = (t: number): number => 1 - (1 - t) * (1 - t) * (1 - t);

export const lerp = (from: number, to: number, t: number): number => from + (to - from) * t;

/** How far a tween that started at `start` is at `time`, from 0 to 1, not eased. */
export const tweenProgress = (time: number, start: number): number =>
  Math.min(1, (time - start) / TWEEN_MS);

/** A rectangle by its corners, in drawing coordinates. */
export type Area = { x0: number; y0: number; x1: number; y1: number };

/** The area kept in the drawing, and the zoom it was worked out at. */
type Kept = Area & { k: number };

const areaAt = (x: number, y: number, size: Size): Area => ({
  x0: x,
  y0: y,
  x1: x + size.w,
  y1: y + size.h,
});

export const contains = (outer: Area, inner: Area): boolean =>
  inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1;

export const overlaps = (a: Area, b: Area): boolean =>
  a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0;

/** What the viewport shows of the drawing. */
export const visibleArea = (view: ViewTransform, viewport: Size): Area => ({
  x0: -view.x / view.k,
  y0: -view.y / view.k,
  x1: (viewport.w - view.x) / view.k,
  y1: (viewport.h - view.y) / view.k,
});

/** `area` and half of it again on every side: a pan inside it culls nothing again. */
export function grown(area: Area): Area {
  const marginX = (area.x1 - area.x0) / 2;
  const marginY = (area.y1 - area.y0) / 2;
  return {
    x0: area.x0 - marginX,
    y0: area.y0 - marginY,
    x1: area.x1 + marginX,
    y1: area.y1 + marginY,
  };
}

/** Where it is and where it is going: a node on its way in is drawn all the way. */
export const nodeShown = (node: SceneNode, kept: Area, pinned: string | undefined): boolean =>
  node.id === pinned ||
  overlaps(areaAt(node.x, node.y, node), kept) ||
  overlaps(areaAt(node.tx, node.ty, node), kept);

/** An edge not drawn yet, or on its way, stays in. */
export const edgeShown = (edge: Pick<SceneEdge, "current" | "moving">, kept: Area): boolean =>
  !edge.current || edge.moving || overlaps(reach(edge.current), kept);

/** The corners of a route's bounding box: its ends and, for a curve, its control points. */
export function reach(route: Route): Area {
  if (route.kind === "poly") {
    const box = { x0: Infinity, y0: Infinity, x1: -Infinity, y1: -Infinity };
    for (const point of route.points) {
      box.x0 = Math.min(box.x0, point.x);
      box.y0 = Math.min(box.y0, point.y);
      box.x1 = Math.max(box.x1, point.x);
      box.y1 = Math.max(box.y1, point.y);
    }
    return box;
  }
  const xs =
    route.kind === "cubic" ? [route.x0, route.c1x, route.c2x, route.x1] : [route.x0, route.x1];
  const ys =
    route.kind === "cubic" ? [route.y0, route.c1y, route.c2y, route.y1] : [route.y0, route.y1];
  return { x0: Math.min(...xs), y0: Math.min(...ys), x1: Math.max(...xs), y1: Math.max(...ys) };
}

/** Shows or hides an item's element, when that changes or when `force`. */
function display(
  item: { shown: boolean },
  shown: boolean,
  force: boolean,
  element: ElementCSSInlineStyle | null | undefined,
): void {
  if (shown === item.shown && !force) return;
  item.shown = shown;
  if (element) element.style.display = shown ? "" : "none";
}

function place(element: Element | undefined, at: Point): void {
  element?.setAttribute("transform", translate(at.x, at.y));
}

const rightMiddle = (box: Box): Point => ({ x: box.x + box.w, y: box.y + box.h / 2 });

/** Corners on their way from `start` to `target`: those tied to a node are already where it is. */
function midCorners(
  start: readonly Point[],
  target: readonly Point[],
  points: readonly RoutePoint[],
  progress: number,
): Point[] {
  return target.map((point, index) => {
    const was = start[index];
    const source = points[index];
    return {
      x: source.xn === undefined ? lerp(was.x, point.x, progress) : point.x,
      y: source.yn === undefined ? lerp(was.y, point.y, progress) : point.y,
    };
  });
}

type Lookup = (id: string) => Point | undefined;

/** A curve's corners now: those tied to a node follow it; the others move from where they were. */
export function cornersOf(
  edge: Pick<SceneEdge, "points" | "start">,
  from: Box,
  to: Box,
  at: Lookup,
  progress: number,
): Point[] {
  if (!edge.points || edge.points.length < 2) return [rightMiddle(from), rightMiddle(to)];
  const target = resolvePoints(edge.points, at);
  const start = edge.start;
  if (!start || progress >= 1 || start.length !== target.length) return target;
  return midCorners(start, target, edge.points, progress);
}

export function routeOf(
  edge: Pick<SceneEdge, "from" | "to" | "route" | "points" | "start">,
  nodes: ReadonlyMap<string, Box>,
  mode: LayoutMode,
  progress: number,
): Route | undefined {
  const from = nodes.get(edge.from);
  const to = nodes.get(edge.to);
  if (!from || !to) return undefined;
  return edge.route === "tree"
    ? treeRoute(mode, from, to)
    : { kind: "poly", points: cornersOf(edge, from, to, (id) => nodes.get(id), progress) };
}

/** Where an edge's count sits. */
export function labelPoint(label: LabelAt | undefined, route: Route): Point {
  if (route.kind !== "poly" || !label) return routePoint(route, 0.5);
  if ("distance" in label) return polyPoint(route.points, label.distance);
  return polyPoint(route.points, label.fraction * polyLength(route.points));
}

const samePoints = (a: readonly Point[], b: readonly Point[]): boolean =>
  a.length === b.length &&
  b.every((point, index) => point.x === a[index].x && point.y === a[index].y);

/**
 * Where a curve's corners start from when a tween moves it to new lanes: from
 * where it is drawn now, mid-tween included. None when its lanes stay.
 */
export function lanesFrom(
  points: readonly RoutePoint[] | undefined,
  drawn: readonly Point[] | undefined,
  targets: Lookup,
): readonly Point[] | undefined {
  if (!drawn || !points) return undefined;
  return samePoints(drawn, resolvePoints(points, targets)) ? undefined : drawn;
}

/** Where a pulse is along its edge, `t` of the way; none once it ended or its edge has no route. */
export const pulsePoint = (route: Route | undefined, t: number): Point | undefined =>
  t >= 1 || !route ? undefined : routePoint(route, t);

export const clampZoom = (k: number): number => Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, k));

export const viewTransform = (view: ViewTransform): string =>
  `translate(${view.x},${view.y}) scale(${view.k})`;

/** No size yet: the pane was not measured. */
const unmeasured = (size: Size): boolean => size.w === 0 || size.h === 0;

/**
 * The first view: readable first, and from the top-left corner, so a tree that
 * grows downwards then moves nothing that is already on screen. None before
 * the pane and the drawing have a size.
 */
export function homeView(
  viewport: Size,
  content: Size,
  homeWidth: number,
): ViewTransform | undefined {
  if (unmeasured(viewport) || content.w === 0) return undefined;
  const wide = homeWidth > 0 ? Math.min(content.w, homeWidth) : content.w;
  const k = Math.min(1, Math.max(HOME_MIN_ZOOM, (viewport.w - 2 * PADDING) / wide));
  return { x: PADDING, y: HOME_TOP, k };
}

/** The whole drawing in view; with `legible`, see `Scene.fit`. */
export function fitTo(content: Size, viewport: Size, legible: boolean): ViewTransform {
  const next = fitView(content, viewport, PADDING, legible ? LEGIBLE_ZOOM : MIN_ZOOM, 1);
  if (legible) next.y = Math.max(next.y, HOME_TOP);
  return next;
}

/** The view zoomed by `factor` about the point (`px`, `py`) of the viewport, which stays where it is. */
export function zoomAbout(
  view: ViewTransform,
  px: number,
  py: number,
  factor: number,
): ViewTransform {
  const k = clampZoom(view.k * factor);
  const ratio = k / view.k;
  return { x: px - (px - view.x) * ratio, y: py - (py - view.y) * ratio, k };
}

/** How far to move a span on screen, from `start` to `end`, to bring it inside `length` and its padding. */
function shiftInto(start: number, end: number, length: number): number {
  if (start < PADDING) return PADDING - start;
  if (end > length - PADDING) return length - PADDING - end;
  return 0;
}

/** The smallest pan that brings `box` into the viewport. */
export function revealShift(
  box: Box,
  view: ViewTransform,
  viewport: Size,
): { dx: number; dy: number } {
  const left = box.x * view.k + view.x;
  const top = box.y * view.k + view.y;
  return {
    dx: shiftInto(left, left + box.w * view.k, viewport.w),
    dy: shiftInto(top, top + box.h * view.k, viewport.h),
  };
}

/** A node that enters the drawing: it flies out of `origin` when there is one. */
export function newNode(
  input: SceneNodeInput,
  origin: Point | undefined,
  el: SVGGElement | undefined,
): SceneNode {
  const start = origin ?? input;
  return {
    ...input,
    x: start.x,
    y: start.y,
    fx: start.x,
    fy: start.y,
    tx: input.x,
    ty: input.y,
    moving: origin !== undefined,
    shown: true,
    el,
  };
}

/** Points a node already drawn at its new place: a tween takes it there, or it is there at once. */
function retarget(node: SceneNode, input: SceneNodeInput, tween: boolean): boolean {
  Object.assign(node, { w: input.w, h: input.h, fx: node.x, fy: node.y, tx: input.x, ty: input.y });
  node.moving = node.x !== input.x || node.y !== input.y;
  if (node.moving && !tween) {
    node.x = input.x;
    node.y = input.y;
    node.moving = false;
    place(node.el, node);
  }
  return node.moving;
}

/** Moves a node `eased` of the way to its place; `t` is 1 at the tween's end. */
function stepNode(node: SceneNode, eased: number, t: number): void {
  node.x = lerp(node.fx, node.tx, eased);
  node.y = lerp(node.fy, node.ty, eased);
  place(node.el, node);
  node.moving = t < 1;
}

/** Where a curve is drawn now, mid-tween included: its new lanes start from there. */
const drawnCorners = (edge: SceneEdge | undefined): readonly Point[] | undefined =>
  edge?.current?.kind === "poly" ? edge.current.points : undefined;

/**
 * Drops what `inputs` no longer holds, then places each input.
 * True when anything is on its way somewhere.
 */
function placeAll<T extends { id: string }>(
  map: Map<string, unknown>,
  inputs: readonly T[],
  placeOne: (input: T) => boolean,
): boolean {
  const wanted = new Set(inputs.map((input) => input.id));
  for (const id of map.keys()) if (!wanted.has(id)) map.delete(id);
  let anyMoving = false;
  for (const input of inputs) anyMoving = placeOne(input) || anyMoving;
  return anyMoving;
}

export type Scene = ReturnType<typeof createScene>;

/**
 * Everything about the drawing that changes from frame to frame, kept out of
 * the reactive graph (spikes/graph-layout): positions on their way to a new
 * layout, pan and zoom, pulses. Solid owns the elements and hands them over;
 * one frame loop writes `transform` and `d` straight to them, and only runs
 * while something moves.
 */
export function createScene(options: SceneOptions) {
  return new LiveScene(options);
}

// A class rather than a closure of methods: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class LiveScene {
  private readonly options: SceneOptions;
  private readonly requestFrame: (run: (time: number) => void) => number;
  private readonly cancelFrame: (handle: number) => void;
  private readonly now: () => number;
  private readonly nodes = new Map<string, SceneNode>();
  private readonly edges = new Map<string, SceneEdge>();
  private readonly pulses = new Map<string, Pulse>();
  private readonly pendingNodeEls = new Map<string, SVGGElement>();
  private readonly pendingEdgeEls = new Map<string, EdgeElements>();
  private mode: LayoutMode = "indented";
  private content: Size = { w: 0, h: 0 };
  private homeWidth = 0;
  /** How far the running tween is, eased: 1 when none runs. */
  private progress = 1;
  private viewport: Size = { w: 0, h: 0 };
  private readonly transform: ViewTransform = { x: 0, y: 0, k: 1 };
  private viewEl: SVGGElement | undefined;
  private lod = false;
  private viewDirty = true;
  private tweenStart: number | undefined;
  private frameHandle: number | undefined;
  private anchor: string | undefined;
  private homed = false;
  // Until the reader pans or zooms, the view is ours to set again when the pane changes size.
  private untouched = true;
  private pulseSeq = 0;
  private disposed = false;
  private kept: Kept = { x0: 0, y0: 0, x1: 0, y1: 0, k: 0 };
  // The node that holds the keyboard's tab stop is never taken out: it must stay focusable.
  private pinned: string | undefined;

  constructor(options: SceneOptions) {
    this.options = options;
    this.requestFrame = options.requestFrame ?? ((run) => requestAnimationFrame(run));
    this.cancelFrame = options.cancelFrame ?? ((handle) => cancelAnimationFrame(handle));
    this.now = options.now ?? (() => performance.now());
  }

  private writeEdge(edge: SceneEdge): void {
    const route = routeOf(edge, this.nodes, this.mode, this.progress);
    edge.current = route;
    if (!route || !edge.els) return;
    const d = routePath(route);
    if (d === edge.written) return;
    edge.written = d;
    edge.els.path?.setAttribute("d", d);
    edge.els.hit?.setAttribute("d", d);
    if (edge.els.label) place(edge.els.label, labelPoint(edge.label, route));
  }

  /**
   * Takes what is far outside the viewport out of the drawing (`display: none`,
   * no element is created or removed), so a pan repaints what can be seen and
   * not the whole graph. The kept area is the viewport and half of it again on
   * every side, worked out again only when the view leaves it.
   */
  private cull(force: boolean): void {
    if (unmeasured(this.viewport)) return;
    const visible = visibleArea(this.transform, this.viewport);
    if (!force && this.transform.k === this.kept.k && contains(this.kept, visible)) return;
    this.kept = { ...grown(visible), k: this.transform.k };
    for (const node of this.nodes.values()) {
      display(node, nodeShown(node, this.kept, this.pinned), force, node.el);
    }
    for (const edge of this.edges.values()) {
      display(edge, edgeShown(edge, this.kept), force, edge.els?.path?.parentElement);
    }
  }

  private writeView(): void {
    this.viewDirty = false;
    this.cull(false);
    this.viewEl?.setAttribute("transform", viewTransform(this.transform));
    const wanted = this.transform.k < LOD_BELOW;
    if (wanted !== this.lod || this.viewEl?.classList.contains("lod") !== wanted) {
      this.lod = wanted;
      this.viewEl?.classList.toggle("lod", wanted);
    }
    this.options.onView?.({ ...this.transform });
  }

  private schedule(): void {
    if (this.frameHandle !== undefined || this.disposed) return;
    this.frameHandle = this.requestFrame(() => this.frame());
  }

  private publishPulses(): void {
    this.options.onPulses([...this.pulses.keys()]);
  }

  private frame(): void {
    this.frameHandle = undefined;
    if (this.disposed) return;
    const time = this.now();
    if (this.tweenStart !== undefined) this.stepTween(tweenProgress(time, this.tweenStart));
    this.stepPulses(time);
    if (this.viewDirty) this.writeView();
    if (this.busy()) this.schedule();
  }

  /** Moves what is on its way `t` of the tween. */
  private stepTween(t: number): void {
    this.progress = ease(t);
    const held = this.anchorAt();
    for (const node of this.nodes.values()) if (node.moving) stepNode(node, this.progress, t);
    for (const edge of this.edges.values()) {
      if (!edge.moving) continue;
      this.writeEdge(edge);
      edge.moving = t < 1;
    }
    this.hold(held);
    if (t >= 1) {
      this.tweenStart = undefined;
      this.progress = 1;
      // What was only drawn because it was moving may now rest outside.
      this.cull(true);
    }
  }

  private anchorAt(): Point | undefined {
    return this.anchor === undefined ? undefined : this.position(this.anchor);
  }

  /** The node under the reader's eye stays where it is on screen: the view follows it. */
  private hold(before: Point | undefined): void {
    const after = this.anchorAt();
    if (!before || !after || (after.x === before.x && after.y === before.y)) return;
    this.transform.x -= (after.x - before.x) * this.transform.k;
    this.transform.y -= (after.y - before.y) * this.transform.k;
    this.viewDirty = true;
  }

  private stepPulses(time: number): void {
    let ended = false;
    for (const pulse of this.pulses.values()) {
      const route = this.edges.get(pulse.edge)?.current;
      const at = pulsePoint(route, (time - pulse.start) / PULSE_MS);
      if (at) place(pulse.el, at);
      else {
        this.pulses.delete(pulse.id);
        ended = true;
      }
    }
    if (ended) this.publishPulses();
  }

  private touchView(): void {
    this.viewDirty = true;
    this.schedule();
  }

  private home(): void {
    const next = homeView(this.viewport, this.content, this.homeWidth);
    if (!next) return;
    this.homed = true;
    Object.assign(this.transform, next);
    this.touchView();
  }

  private placeNode(input: SceneNodeInput, tween: boolean): boolean {
    const existing = this.nodes.get(input.id);
    if (existing) return retarget(existing, input, tween);
    // A newcomer flies out of the node that started it.
    const origin = tween && input.origin !== undefined ? this.nodes.get(input.origin) : undefined;
    const node = newNode(input, origin, this.pendingNodeEls.get(input.id));
    this.nodes.set(input.id, node);
    place(node.el, node);
    return node.moving;
  }

  private placeEdge(input: SceneEdgeInput, tween: boolean): boolean {
    const existing = this.edges.get(input.id);
    const drawn = drawnCorners(existing);
    const edge: SceneEdge = existing
      ? Object.assign(existing, input)
      : { ...input, moving: false, shown: true, els: this.pendingEdgeEls.get(input.id) };
    this.edges.set(input.id, edge);
    edge.start = tween ? lanesFrom(edge.points, drawn, (id) => this.target(id)) : undefined;
    edge.moving = edge.start !== undefined || this.endsMoving(edge);
    return edge.moving;
  }

  /** Where a node is going. */
  private target(id: string): Point | undefined {
    const node = this.nodes.get(id);
    return node ? { x: node.tx, y: node.ty } : undefined;
  }

  private endsMoving(edge: SceneEdge): boolean {
    return [edge.from, edge.to].some((id) => this.nodes.get(id)?.moving === true);
  }

  /** The group that holds the drawing: pan and zoom are its `transform`. */
  mount(element: SVGGElement): void {
    this.viewEl = element;
    this.writeView();
  }

  nodeRef(id: string, element: SVGGElement): void {
    const node = this.nodes.get(id);
    if (!node) {
      // The element came before the layout that places it.
      this.pendingNodeEls.set(id, element);
      return;
    }
    node.el = element;
    place(element, node);
    if (!node.shown) element.style.display = "none";
  }

  edgeRef(id: string, part: EdgePart, element: SVGPathElement | SVGGElement): void {
    const edge = this.edges.get(id);
    const elements = edge ? (edge.els ??= {}) : (this.pendingEdgeEls.get(id) ?? {});
    Object.assign(elements, { [part]: element });
    if (edge) {
      // A new element has no path yet, whatever was written to the others.
      edge.written = undefined;
      this.writeEdge(edge);
    } else this.pendingEdgeEls.set(id, elements);
  }

  pulseRef(id: string, element: SVGCircleElement): void {
    const pulse = this.pulses.get(id);
    if (!pulse) return;
    pulse.el = element;
    const route = this.edges.get(pulse.edge)?.current;
    if (route) place(element, routePoint(route, 0));
  }

  /** Takes a new layout: what moved is tweened to its place, unless `animate` is false. */
  setLayout(layout: SceneLayout, animate: boolean): void {
    const tween = animate && !this.options.reducedMotion() && this.nodes.size > 0;
    this.mode = layout.mode;
    this.content = { w: layout.width, h: layout.height };
    this.homeWidth = layout.homeWidth ?? 0;
    const nodesMoving = placeAll(this.nodes, layout.nodes, (input) => this.placeNode(input, tween));
    const edgesMoving = placeAll(this.edges, layout.edges, (input) => this.placeEdge(input, tween));
    const anyMoving = nodesMoving || edgesMoving;
    this.progress = anyMoving ? 0 : 1;
    for (const edge of this.edges.values()) this.writeEdge(edge);
    this.cull(true);
    if (anyMoving) {
      this.tweenStart = this.now();
      this.schedule();
    }
    // Every element this layout wanted is taken: the rest belong to nodes and edges that came and went.
    this.pendingNodeEls.clear();
    this.pendingEdgeEls.clear();
    if (!this.homed) this.home();
  }

  setViewport(size: Size): void {
    const resized = size.w !== this.viewport.w || size.h !== this.viewport.h;
    this.viewport = size;
    if (!this.homed || (this.untouched && resized)) this.home();
    if (resized) this.cull(true);
  }

  /** Names the node that must stay in the drawing wherever it is: the keyboard's tab stop. */
  pin(id: string | undefined): void {
    if (id === this.pinned) return;
    this.pinned = id;
    this.cull(true);
  }

  /** Keeps this node where it is on screen while a layout moves it. */
  setAnchor(id: string | undefined): void {
    this.anchor = id;
  }

  panBy(dx: number, dy: number): void {
    this.untouched = false;
    this.transform.x += dx;
    this.transform.y += dy;
    this.touchView();
  }

  /** Zooms by `factor` keeping the point (`px`, `py`) of the viewport where it is. */
  zoomAt(px: number, py: number, factor: number): void {
    this.untouched = false;
    Object.assign(this.transform, zoomAbout(this.transform, px, py, factor));
    this.touchView();
  }

  zoomBy(factor: number): void {
    this.zoomAt(this.viewport.w / 2, this.viewport.h / 2, factor);
  }

  /**
   * Shows the whole drawing. With `legible`, never below the zoom at which
   * cards keep their text, and never under the buttons that float over the
   * canvas: a drawing that is then too big starts at its top-left corner.
   */
  fit(legible = false): void {
    if (unmeasured(this.viewport)) return;
    this.homed = true;
    this.untouched = false;
    Object.assign(this.transform, fitTo(this.content, this.viewport, legible));
    this.touchView();
  }

  /** Pans just enough to bring a node into the viewport. */
  reveal(id: string): void {
    const node = this.nodes.get(id);
    if (!node || this.viewport.w === 0) return;
    const target = { x: node.tx, y: node.ty, w: node.w, h: node.h };
    const { dx, dy } = revealShift(target, this.transform, this.viewport);
    if (dx !== 0 || dy !== 0) this.panBy(dx, dy);
    // At once, not at the next frame: the node is about to take the focus.
    this.cull(false);
  }

  /** Sends one pulse along an edge. Nothing when motion is to be reduced. */
  pulse(edgeId: string): void {
    if (this.options.reducedMotion() || !this.edges.has(edgeId)) return;
    this.pulseSeq += 1;
    const id = `pulse-${this.pulseSeq}`;
    this.pulses.set(id, { id, edge: edgeId, start: this.now() });
    this.publishPulses();
    this.schedule();
  }

  view(): ViewTransform {
    return { ...this.transform };
  }

  /** Where a node is right now, mid-tween included. */
  position(id: string): { x: number; y: number } | undefined {
    const node = this.nodes.get(id);
    return node ? { x: node.x, y: node.y } : undefined;
  }

  /** True while a tween or a pulse is running. */
  busy(): boolean {
    return this.tweenStart !== undefined || this.pulses.size > 0;
  }

  dispose(): void {
    this.disposed = true;
    if (this.frameHandle !== undefined) this.cancelFrame(this.frameHandle);
    this.frameHandle = undefined;
  }
}
