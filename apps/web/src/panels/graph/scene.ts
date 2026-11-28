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

/** Removes every entry whose id is not in `ids`. */
function keepOnly(map: Map<string, unknown>, ids: readonly string[]): void {
  const wanted = new Set(ids);
  for (const id of map.keys()) if (!wanted.has(id)) map.delete(id);
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
  const requestFrame = options.requestFrame ?? ((run) => requestAnimationFrame(run));
  const cancelFrame = options.cancelFrame ?? ((handle) => cancelAnimationFrame(handle));
  const now = options.now ?? (() => performance.now());

  const nodes = new Map<string, SceneNode>();
  const edges = new Map<string, SceneEdge>();
  const pulses = new Map<string, Pulse>();
  const pendingNodeEls = new Map<string, SVGGElement>();
  const pendingEdgeEls = new Map<string, EdgeElements>();
  let mode: LayoutMode = "indented";
  let content: Size = { w: 0, h: 0 };
  let homeWidth = 0;
  /** How far the running tween is, eased: 1 when none runs. */
  let progress = 1;
  let viewport: Size = { w: 0, h: 0 };
  const view: ViewTransform = { x: 0, y: 0, k: 1 };
  let viewEl: SVGGElement | undefined;
  let lod = false;
  let viewDirty = true;
  let tweenStart: number | undefined;
  let frameHandle: number | undefined;
  let anchor: string | undefined;
  let homed = false;
  // Until the reader pans or zooms, the view is ours to set again when the pane changes size.
  let untouched = true;
  let pulseSeq = 0;
  let disposed = false;
  let kept: Kept = { x0: 0, y0: 0, x1: 0, y1: 0, k: 0 };
  // The node that holds the keyboard's tab stop is never taken out: it must stay focusable.
  let pinned: string | undefined;

  function writeEdge(edge: SceneEdge): void {
    const route = routeOf(edge, nodes, mode, progress);
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
  function cull(force: boolean): void {
    if (unmeasured(viewport)) return;
    const visible = visibleArea(view, viewport);
    if (!force && view.k === kept.k && contains(kept, visible)) return;
    kept = { ...grown(visible), k: view.k };
    for (const node of nodes.values()) {
      display(node, nodeShown(node, kept, pinned), force, node.el);
    }
    for (const edge of edges.values()) {
      display(edge, edgeShown(edge, kept), force, edge.els?.path?.parentElement);
    }
  }

  function writeView(): void {
    viewDirty = false;
    cull(false);
    viewEl?.setAttribute("transform", viewTransform(view));
    const wanted = view.k < LOD_BELOW;
    if (wanted !== lod || viewEl?.classList.contains("lod") !== wanted) {
      lod = wanted;
      viewEl?.classList.toggle("lod", wanted);
    }
    options.onView?.({ ...view });
  }

  function schedule(): void {
    if (frameHandle !== undefined || disposed) return;
    frameHandle = requestFrame(frame);
  }

  function publishPulses(): void {
    options.onPulses([...pulses.keys()]);
  }

  function frame(): void {
    frameHandle = undefined;
    if (disposed) return;
    const time = now();
    if (tweenStart !== undefined) {
      const t = tweenProgress(time, tweenStart);
      const eased = ease(t);
      progress = eased;
      const held = anchor === undefined ? undefined : nodes.get(anchor);
      const heldAt = held ? { x: held.x, y: held.y } : undefined;
      for (const node of nodes.values()) {
        if (!node.moving) continue;
        node.x = lerp(node.fx, node.tx, eased);
        node.y = lerp(node.fy, node.ty, eased);
        place(node.el, node);
        if (t >= 1) node.moving = false;
      }
      for (const edge of edges.values()) {
        if (!edge.moving) continue;
        writeEdge(edge);
        if (t >= 1) edge.moving = false;
      }
      // The node under the reader's eye stays where it is on screen: the view follows it.
      if (held && heldAt && (held.x !== heldAt.x || held.y !== heldAt.y)) {
        view.x -= (held.x - heldAt.x) * view.k;
        view.y -= (held.y - heldAt.y) * view.k;
        viewDirty = true;
      }
      if (t >= 1) {
        tweenStart = undefined;
        progress = 1;
        // What was only drawn because it was moving may now rest outside.
        cull(true);
      }
    }
    let ended = false;
    for (const pulse of pulses.values()) {
      const at = pulsePoint(edges.get(pulse.edge)?.current, (time - pulse.start) / PULSE_MS);
      if (at) place(pulse.el, at);
      else {
        pulses.delete(pulse.id);
        ended = true;
      }
    }
    if (ended) publishPulses();
    if (viewDirty) writeView();
    if (tweenStart !== undefined || pulses.size > 0) schedule();
  }

  function touchView(): void {
    viewDirty = true;
    schedule();
  }

  function home(): void {
    const next = homeView(viewport, content, homeWidth);
    if (!next) return;
    homed = true;
    Object.assign(view, next);
    touchView();
  }

  return {
    /** The group that holds the drawing: pan and zoom are its `transform`. */
    mount(element: SVGGElement): void {
      viewEl = element;
      writeView();
    },
    nodeRef(id: string, element: SVGGElement): void {
      const node = nodes.get(id);
      if (!node) {
        // The element came before the layout that places it.
        pendingNodeEls.set(id, element);
        return;
      }
      node.el = element;
      place(element, node);
      if (!node.shown) element.style.display = "none";
    },
    edgeRef(id: string, part: EdgePart, element: SVGPathElement | SVGGElement): void {
      const edge = edges.get(id);
      const elements = edge ? (edge.els ??= {}) : (pendingEdgeEls.get(id) ?? {});
      Object.assign(elements, { [part]: element });
      if (edge) {
        // A new element has no path yet, whatever was written to the others.
        edge.written = undefined;
        writeEdge(edge);
      } else pendingEdgeEls.set(id, elements);
    },
    pulseRef(id: string, element: SVGCircleElement): void {
      const pulse = pulses.get(id);
      if (!pulse) return;
      pulse.el = element;
      const route = edges.get(pulse.edge)?.current;
      if (route) place(element, routePoint(route, 0));
    },
    /** Takes a new layout: what moved is tweened to its place, unless `animate` is false. */
    setLayout(layout: SceneLayout, animate: boolean): void {
      const tween = animate && !options.reducedMotion() && nodes.size > 0;
      mode = layout.mode;
      content = { w: layout.width, h: layout.height };
      homeWidth = layout.homeWidth ?? 0;
      keepOnly(
        nodes,
        layout.nodes.map((node) => node.id),
      );
      let anyMoving = false;
      for (const input of layout.nodes) {
        const existing = nodes.get(input.id);
        if (existing) {
          existing.w = input.w;
          existing.h = input.h;
          existing.fx = existing.x;
          existing.fy = existing.y;
          existing.tx = input.x;
          existing.ty = input.y;
          existing.moving = existing.x !== input.x || existing.y !== input.y;
          if (existing.moving && !tween) {
            existing.x = input.x;
            existing.y = input.y;
            existing.moving = false;
            place(existing.el, existing);
          }
          anyMoving ||= existing.moving;
          continue;
        }
        // A newcomer flies out of the node that started it.
        const origin = tween && input.origin !== undefined ? nodes.get(input.origin) : undefined;
        const node = newNode(input, origin, pendingNodeEls.get(input.id));
        pendingNodeEls.delete(input.id);
        nodes.set(input.id, node);
        place(node.el, node);
        anyMoving ||= node.moving;
      }
      keepOnly(
        edges,
        layout.edges.map((edge) => edge.id),
      );
      const targets = (id: string): Point | undefined => {
        const node = nodes.get(id);
        return node ? { x: node.tx, y: node.ty } : undefined;
      };
      for (const input of layout.edges) {
        const existing = edges.get(input.id);
        const drawn = existing?.current?.kind === "poly" ? existing.current.points : undefined;
        const edge: SceneEdge = existing
          ? Object.assign(existing, input)
          : { ...input, moving: false, shown: true, els: pendingEdgeEls.get(input.id) };
        pendingEdgeEls.delete(input.id);
        edges.set(input.id, edge);
        const ends = [nodes.get(edge.from), nodes.get(edge.to)];
        edge.start = tween ? lanesFrom(edge.points, drawn, targets) : undefined;
        edge.moving = edge.start !== undefined || ends.some((end) => end?.moving === true);
        anyMoving ||= edge.moving;
      }
      progress = anyMoving ? 0 : 1;
      for (const edge of edges.values()) writeEdge(edge);
      cull(true);
      if (anyMoving) {
        tweenStart = now();
        schedule();
      }
      // Every element this layout wanted is taken: the rest belong to nodes and edges that came and went.
      pendingNodeEls.clear();
      pendingEdgeEls.clear();
      if (!homed) home();
    },
    setViewport(size: Size): void {
      const resized = size.w !== viewport.w || size.h !== viewport.h;
      viewport = size;
      if (!homed || (untouched && resized)) home();
      if (resized) cull(true);
    },
    /** Names the node that must stay in the drawing wherever it is: the keyboard's tab stop. */
    pin(id: string | undefined): void {
      if (id === pinned) return;
      pinned = id;
      cull(true);
    },
    /** Keeps this node where it is on screen while a layout moves it. */
    setAnchor(id: string | undefined): void {
      anchor = id;
    },
    panBy(dx: number, dy: number): void {
      untouched = false;
      view.x += dx;
      view.y += dy;
      touchView();
    },
    /** Zooms by `factor` keeping the point (`px`, `py`) of the viewport where it is. */
    zoomAt(px: number, py: number, factor: number): void {
      untouched = false;
      Object.assign(view, zoomAbout(view, px, py, factor));
      touchView();
    },
    zoomBy(factor: number): void {
      this.zoomAt(viewport.w / 2, viewport.h / 2, factor);
    },
    /**
     * Shows the whole drawing. With `legible`, never below the zoom at which
     * cards keep their text, and never under the buttons that float over the
     * canvas: a drawing that is then too big starts at its top-left corner.
     */
    fit(legible = false): void {
      if (unmeasured(viewport)) return;
      homed = true;
      untouched = false;
      Object.assign(view, fitTo(content, viewport, legible));
      touchView();
    },
    /** Pans just enough to bring a node into the viewport. */
    reveal(id: string): void {
      const node = nodes.get(id);
      if (!node || viewport.w === 0) return;
      const { dx, dy } = revealShift(
        { x: node.tx, y: node.ty, w: node.w, h: node.h },
        view,
        viewport,
      );
      if (dx !== 0 || dy !== 0) this.panBy(dx, dy);
      // At once, not at the next frame: the node is about to take the focus.
      cull(false);
    },
    /** Sends one pulse along an edge. Nothing when motion is to be reduced. */
    pulse(edgeId: string): void {
      if (options.reducedMotion() || !edges.has(edgeId)) return;
      pulseSeq += 1;
      const id = `pulse-${pulseSeq}`;
      pulses.set(id, { id, edge: edgeId, start: now() });
      publishPulses();
      schedule();
    },
    view: (): ViewTransform => ({ ...view }),
    /** Where a node is right now, mid-tween included. */
    position(id: string): { x: number; y: number } | undefined {
      const node = nodes.get(id);
      return node ? { x: node.x, y: node.y } : undefined;
    },
    /** True while a tween or a pulse is running. */
    busy: (): boolean => tweenStart !== undefined || pulses.size > 0,
    dispose(): void {
      disposed = true;
      if (frameHandle !== undefined) cancelFrame(frameHandle);
      frameHandle = undefined;
    },
  };
}
