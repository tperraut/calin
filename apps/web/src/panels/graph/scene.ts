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
  let kept = { x0: 0, y0: 0, x1: 0, y1: 0, k: 0 };
  // The node that holds the keyboard's tab stop is never taken out: it must stay focusable.
  let pinned: string | undefined;

  const box = (node: SceneNode): Box => node;

  const at = (id: string): SceneNode | undefined => nodes.get(id);

  /** A curve's corners now: those tied to a node follow it; the others move from where they were. */
  function cornersOf(edge: SceneEdge, from: SceneNode, to: SceneNode): Point[] {
    if (!edge.points || edge.points.length < 2) {
      return [
        { x: from.x + from.w, y: from.y + from.h / 2 },
        { x: to.x + to.w, y: to.y + to.h / 2 },
      ];
    }
    const target = resolvePoints(edge.points, at);
    const start = edge.start;
    if (!start || progress >= 1 || start.length !== target.length) return target;
    return target.map((point, index) => {
      const was = start[index];
      const source = edge.points?.[index];
      return {
        x: source?.xn === undefined ? was.x + (point.x - was.x) * progress : point.x,
        y: source?.yn === undefined ? was.y + (point.y - was.y) * progress : point.y,
      };
    });
  }

  function routeOf(edge: SceneEdge): Route | undefined {
    const from = nodes.get(edge.from);
    const to = nodes.get(edge.to);
    if (!from || !to) return undefined;
    return edge.route === "tree"
      ? treeRoute(mode, box(from), box(to))
      : { kind: "poly", points: cornersOf(edge, from, to) };
  }

  /** Where an edge's count sits. */
  function labelPoint(edge: SceneEdge, route: Route): Point {
    if (route.kind !== "poly" || !edge.label) return routePoint(route, 0.5);
    if ("distance" in edge.label) return polyPoint(route.points, edge.label.distance);
    return polyPoint(route.points, edge.label.fraction * polyLength(route.points));
  }

  function writeNode(node: SceneNode): void {
    node.el?.setAttribute("transform", translate(node.x, node.y));
  }

  function writeEdge(edge: SceneEdge): void {
    const route = routeOf(edge);
    edge.current = route;
    if (!route || !edge.els) return;
    const d = routePath(route);
    if (d === edge.written) return;
    edge.written = d;
    edge.els.path?.setAttribute("d", d);
    edge.els.hit?.setAttribute("d", d);
    if (edge.els.label) {
      const middle = labelPoint(edge, route);
      edge.els.label.setAttribute("transform", translate(middle.x, middle.y));
    }
  }

  /** The corners of a route's bounding box: its ends and, for a curve, its control points. */
  function reach(route: Route): { x0: number; y0: number; x1: number; y1: number } {
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

  /**
   * Takes what is far outside the viewport out of the drawing (`display: none`,
   * no element is created or removed), so a pan repaints what can be seen and
   * not the whole graph. The kept area is the viewport and half of it again on
   * every side, worked out again only when the view leaves it.
   */
  function cull(force: boolean): void {
    if (viewport.w === 0 || viewport.h === 0) return;
    const left = -view.x / view.k;
    const top = -view.y / view.k;
    const right = (viewport.w - view.x) / view.k;
    const bottom = (viewport.h - view.y) / view.k;
    const inside =
      view.k === kept.k &&
      left >= kept.x0 &&
      top >= kept.y0 &&
      right <= kept.x1 &&
      bottom <= kept.y1;
    if (!force && inside) return;
    const marginX = (right - left) / 2;
    const marginY = (bottom - top) / 2;
    kept = {
      x0: left - marginX,
      y0: top - marginY,
      x1: right + marginX,
      y1: bottom + marginY,
      k: view.k,
    };
    const within = (x0: number, y0: number, x1: number, y1: number): boolean =>
      x0 < kept.x1 && x1 > kept.x0 && y0 < kept.y1 && y1 > kept.y0;
    for (const node of nodes.values()) {
      // Where it is and where it is going: a node on its way in is drawn all the way.
      const shown =
        node.id === pinned ||
        within(node.x, node.y, node.x + node.w, node.y + node.h) ||
        within(node.tx, node.ty, node.tx + node.w, node.ty + node.h);
      if (shown !== node.shown || force) {
        node.shown = shown;
        if (node.el) node.el.style.display = shown ? "" : "none";
      }
    }
    for (const edge of edges.values()) {
      const box = edge.current ? reach(edge.current) : undefined;
      const shown = box === undefined || edge.moving || within(box.x0, box.y0, box.x1, box.y1);
      if (shown !== edge.shown || force) {
        edge.shown = shown;
        const group = edge.els?.path?.parentElement;
        if (group) group.style.display = shown ? "" : "none";
      }
    }
  }

  function writeView(): void {
    viewDirty = false;
    cull(false);
    viewEl?.setAttribute("transform", `translate(${view.x},${view.y}) scale(${view.k})`);
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
      const t = Math.min(1, (time - tweenStart) / TWEEN_MS);
      const eased = ease(t);
      progress = eased;
      const held = anchor === undefined ? undefined : nodes.get(anchor);
      const heldAt = held ? { x: held.x, y: held.y } : undefined;
      for (const node of nodes.values()) {
        if (!node.moving) continue;
        node.x = node.fx + (node.tx - node.fx) * eased;
        node.y = node.fy + (node.ty - node.fy) * eased;
        writeNode(node);
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
    if (pulses.size > 0) {
      let ended = false;
      for (const pulse of pulses.values()) {
        const t = (time - pulse.start) / PULSE_MS;
        const route = edges.get(pulse.edge)?.current;
        if (t >= 1 || !route) {
          pulses.delete(pulse.id);
          ended = true;
          continue;
        }
        const at = routePoint(route, t);
        pulse.el?.setAttribute("transform", translate(at.x, at.y));
      }
      if (ended) publishPulses();
    }
    if (viewDirty) writeView();
    if (tweenStart !== undefined || pulses.size > 0) schedule();
  }

  function clampZoom(k: number): number {
    return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, k));
  }

  function touchView(): void {
    viewDirty = true;
    schedule();
  }

  function home(): void {
    if (viewport.w === 0 || viewport.h === 0 || content.w === 0) return;
    homed = true;
    // Readable first, and from the top-left corner: a tree that grows downwards
    // then moves nothing that is already on screen.
    const wide = homeWidth > 0 ? Math.min(content.w, homeWidth) : content.w;
    const k = Math.min(1, Math.max(HOME_MIN_ZOOM, (viewport.w - 2 * PADDING) / wide));
    Object.assign(view, { x: PADDING, y: HOME_TOP, k });
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
      writeNode(node);
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
      if (route) {
        const at = routePoint(route, 0);
        element.setAttribute("transform", translate(at.x, at.y));
      }
    },
    /** Takes a new layout: what moved is tweened to its place, unless `animate` is false. */
    setLayout(layout: SceneLayout, animate: boolean): void {
      const tween = animate && !options.reducedMotion() && nodes.size > 0;
      mode = layout.mode;
      content = { w: layout.width, h: layout.height };
      homeWidth = layout.homeWidth ?? 0;
      const wanted = new Set(layout.nodes.map((node) => node.id));
      for (const id of nodes.keys()) if (!wanted.has(id)) nodes.delete(id);
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
            writeNode(existing);
          }
          anyMoving ||= existing.moving;
          continue;
        }
        // A newcomer flies out of the node that started it.
        const origin = tween && input.origin !== undefined ? nodes.get(input.origin) : undefined;
        const start = origin ? { x: origin.x, y: origin.y } : { x: input.x, y: input.y };
        const node: SceneNode = {
          ...input,
          x: start.x,
          y: start.y,
          fx: start.x,
          fy: start.y,
          tx: input.x,
          ty: input.y,
          moving: origin !== undefined,
          shown: true,
          el: pendingNodeEls.get(input.id),
        };
        pendingNodeEls.delete(input.id);
        nodes.set(input.id, node);
        writeNode(node);
        anyMoving ||= node.moving;
      }
      const wantedEdges = new Set(layout.edges.map((edge) => edge.id));
      for (const id of edges.keys()) if (!wantedEdges.has(id)) edges.delete(id);
      for (const input of layout.edges) {
        const existing = edges.get(input.id);
        // Where a curve is drawn now, mid-tween included: its new lanes start from there.
        const shown = existing?.current?.kind === "poly" ? existing.current.points : undefined;
        const edge: SceneEdge = existing
          ? Object.assign(existing, input)
          : { ...input, moving: false, shown: true, els: pendingEdgeEls.get(input.id) };
        pendingEdgeEls.delete(input.id);
        edges.set(input.id, edge);
        const ends = [nodes.get(edge.from), nodes.get(edge.to)];
        edge.moving = ends.some((end) => end?.moving === true);
        edge.start = undefined;
        if (tween && shown && edge.points) {
          const target = resolvePoints(edge.points, (id) => {
            const node = nodes.get(id);
            return node ? { x: node.tx, y: node.ty } : undefined;
          });
          const lanesMoved =
            shown.length !== target.length ||
            target.some((point, index) => point.x !== shown[index].x || point.y !== shown[index].y);
          if (lanesMoved) {
            edge.start = shown;
            edge.moving = true;
          }
        }
        anyMoving ||= edge.moving;
      }
      progress = anyMoving ? 0 : 1;
      for (const edge of edges.values()) writeEdge(edge);
      cull(true);
      if (anyMoving) {
        tweenStart = now();
        schedule();
      }
      // Elements of nodes and edges that came and went between two layouts.
      for (const id of pendingNodeEls.keys()) if (!wanted.has(id)) pendingNodeEls.delete(id);
      for (const id of pendingEdgeEls.keys()) {
        if (!wantedEdges.has(id)) pendingEdgeEls.delete(id);
      }
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
      const k = clampZoom(view.k * factor);
      const ratio = k / view.k;
      view.x = px - (px - view.x) * ratio;
      view.y = py - (py - view.y) * ratio;
      view.k = k;
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
      if (viewport.w === 0 || viewport.h === 0) return;
      homed = true;
      untouched = false;
      const next = fitView(content, viewport, PADDING, legible ? LEGIBLE_ZOOM : MIN_ZOOM, 1);
      if (legible) next.y = Math.max(next.y, HOME_TOP);
      Object.assign(view, next);
      touchView();
    },
    /** Pans just enough to bring a node into the viewport. */
    reveal(id: string): void {
      const node = nodes.get(id);
      if (!node || viewport.w === 0) return;
      const left = node.tx * view.k + view.x;
      const top = node.ty * view.k + view.y;
      const right = left + node.w * view.k;
      const bottom = top + node.h * view.k;
      let dx = 0;
      let dy = 0;
      if (left < PADDING) dx = PADDING - left;
      else if (right > viewport.w - PADDING) dx = viewport.w - PADDING - right;
      if (top < PADDING) dy = PADDING - top;
      else if (bottom > viewport.h - PADDING) dy = viewport.h - PADDING - bottom;
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
