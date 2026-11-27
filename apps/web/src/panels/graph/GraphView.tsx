import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  onSettled,
  Show,
} from "solid-js";
import type { EdgeKind } from "../../gen/EdgeKind";
import type { Graph } from "../../gen/Graph";
import { createMediaQuery } from "../../lib/media";
import type { Tone } from "../../lib/status";
import { layoutGraph, NODE_SIZE, sizeOf, type Layout, type LayoutMode } from "./layout";
import {
  clip,
  clipMiddle,
  describeGraph,
  edgeEmphasis,
  edgeKindWords,
  edgeSentence,
  graphModel,
  nodeSentence,
  type EdgeView,
  type GraphModel,
  type NodeView,
  type OutlineItem,
} from "./model";
import { routeOverlay, type RouteBox, type Routing } from "./routing";
import { createScene, type Scene, type SceneLayout } from "./scene";

/** What the reader picked in the graph. */
export type GraphSelection = { kind: "node"; id: string } | { kind: "edge"; id: string };

export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

const ZOOM_STEP = 1.3;
/** A pointer that moved less than this was a tap, not a drag. */
const DRAG_SLOP = 4;

const markPaths: Record<Tone, { d: string; filled: boolean }> = {
  active: { d: "M5,0.5a4.5,4.5 0 1,0 0.01,0z", filled: true },
  attention: { d: "M5,0L10,5L5,10L0,5z", filled: true },
  neutral: { d: "M5,1a4,4 0 1,0 0.01,0z", filled: false },
  success: { d: "M1,5.5L4,8.5L9,1.5", filled: false },
  danger: { d: "M1.5,1.5L8.5,8.5M8.5,1.5L1.5,8.5", filled: false },
};

/** The status as a shape as well as a color; the word is beside it. */
function StatusMark(props: { tone: Tone; x: number; y: number }) {
  const mark = () => markPaths[props.tone] ?? markPaths.neutral;
  return (
    <g class="gmark" data-tone={props.tone} transform={`translate(${props.x},${props.y})`}>
      <path
        d={mark().d}
        fill={mark().filled ? "currentColor" : "none"}
        stroke={mark().filled ? "none" : "currentColor"}
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </g>
  );
}

const edgeKinds = ["spawned", "dispatched", "message", "report", "changed"] as const;

/**
 * How strongly an edge's arrowhead is drawn, with its line: `quiet` while
 * many edges share the drawing, `faint` while another node's or edge's are
 * brought out. The tree's own edges are never faded.
 */
const markerTones = { full: 1, quiet: 0.6, faint: 0.22 } as const;
type MarkerTone = keyof typeof markerTones;
const markerToneNames = Object.keys(markerTones) as MarkerTone[];
/** Lighter arrowheads are smaller too: where many lines meet one card they overlap less. */
const markerScale: Readonly<Record<MarkerTone, number>> = { full: 1, quiet: 0.8, faint: 0.8 };
const markerSpecs: readonly { kind: EdgeKind; which: MarkerTone }[] = edgeKinds.flatMap((kind) =>
  markerToneNames.map((which) => ({ kind, which })),
);

/** With more edges over the tree than this, they are drawn thinner and lighter until asked for. */
export const QUIET_ABOVE = 5;
/** How much of the gutter right of the tree the first view keeps readable. */
const HOME_GUTTER = 64;

/**
 * What an edge ends with, one shape per kind, so the kinds are told apart
 * without color: a triangle for the tree's own edges (larger for a dispatch),
 * an open chevron for notes, a diamond for a report, a dot for changed files.
 */
const arrowheads: Record<
  EdgeKind,
  { d: string; size: number; refX: number; color: string; open?: boolean }
> = {
  spawned: { d: "M0,0.5L8,4L0,7.5z", size: 7, refX: 7, color: "var(--muted)" },
  dispatched: { d: "M0,0.5L8,4L0,7.5z", size: 9, refX: 7, color: "var(--fg)" },
  message: { d: "M1,1L7,4L1,7", size: 8, refX: 7, color: "var(--tone-active)", open: true },
  report: { d: "M0,4L4,0.5L8,4L4,7.5z", size: 10, refX: 8, color: "var(--tone-success)" },
  changed: { d: "M4,1a3,3 0 1,0 0.01,0z", size: 9, refX: 7, color: "var(--tone-attention)" },
};

const AGENT = NODE_SIZE.agent;
const USER = NODE_SIZE.user;
const REPO = NODE_SIZE.repo;
// A folder: a tab on the top-left corner tells a repo from an agent's card at any zoom.
const REPO_SHAPE = `M0,8Q0,2 6,2H44L52,10H${REPO.w - 6}Q${REPO.w},10 ${REPO.w},16V${REPO.h - 6}Q${REPO.w},${REPO.h} ${REPO.w - 6},${REPO.h}H6Q0,${REPO.h} 0,${REPO.h - 6}Z`;

function NodeShape(props: { node: NodeView }) {
  const kind = () => props.node.node.kind;
  const second = (): string => {
    const status = props.node.status?.label ?? "";
    const parts = [status, props.node.role].filter((part) => part !== "");
    return clip(parts.join(" · "), 24);
  };
  return (
    <Show
      when={kind() === "agent"}
      fallback={
        <Show
          when={kind() === "repo"}
          fallback={
            <>
              <rect class="gfocus" x="-4" y="-4" width={USER.w + 8} height={USER.h + 8} rx="24" />
              <rect class="gshape" width={USER.w} height={USER.h} rx={USER.h / 2} />
              <circle cx="20" cy="16.5" r="4.5" fill="var(--accent)" />
              <path d="M11,32a9,8 0 0,1 18,0z" fill="var(--accent)" />
              <text class="gtitle" x="36" y="26.5">
                {clip(props.node.title, 8)}
              </text>
            </>
          }
        >
          <rect class="gfocus" x="-4" y="-2" width={REPO.w + 8} height={REPO.h + 6} rx="8" />
          <path class="gshape" d={REPO_SHAPE} />
          <text class="gtitle" x="10" y="25">
            {clip(props.node.title, 17)}
          </text>
          <text class="gsub" x="10" y="38">
            {clip(props.node.role, 20)}
          </text>
        </Show>
      }
    >
      <rect class="gfocus" x="-5" y="-5" width={AGENT.w + 10} height={AGENT.h + 10} rx="12" />
      <rect class="gown" x="-3" y="-3" width={AGENT.w + 6} height={AGENT.h + 6} rx="10" />
      <rect class="gshape" width={AGENT.w} height={AGENT.h} rx="8" />
      <text class="gtitle" x="10" y="20">
        {clipMiddle(props.node.title, props.node.cost === undefined ? 22 : 14)}
      </text>
      <Show when={props.node.status}>
        {(status) => <StatusMark tone={status().tone} x={10} y={30} />}
      </Show>
      <text class="gsub" x={props.node.status ? 25 : 10} y="39">
        {second()}
      </text>
      <Show when={props.node.cost}>
        {(cost) => (
          <text class="gcost" x={AGENT.w - 10} y="20" text-anchor="end">
            {cost()}
          </text>
        )}
      </Show>
    </Show>
  );
}

function Outline(props: { items: readonly OutlineItem[] }) {
  return (
    <ul>
      <For each={props.items} keyed={(item) => item.id}>
        {(item) => (
          <li>
            {item().text}
            <Show when={item().children.length > 0}>
              <Outline items={item().children} />
            </Show>
          </li>
        )}
      </For>
    </ul>
  );
}

/** Where every card is, for the router. */
function boxesOf(layout: Layout, model: GraphModel): Map<string, RouteBox> {
  const boxes = new Map<string, RouteBox>();
  for (const node of model.nodes) {
    const at = layout.positions.get(node.id);
    if (at) boxes.set(node.id, { ...at, ...sizeOf(node.node), kind: node.node.kind });
  }
  return boxes;
}

function sceneLayout(layout: Layout, model: GraphModel, routing: Routing): SceneLayout {
  const width = Math.max(layout.width, routing.right);
  return {
    mode: layout.mode,
    width,
    height: layout.height,
    homeWidth: Math.min(width, layout.width + HOME_GUTTER),
    nodes: model.nodes.flatMap((node) => {
      const at = layout.positions.get(node.id);
      if (!at) return [];
      return [{ id: node.id, ...at, ...sizeOf(node.node), origin: layout.parent.get(node.id) }];
    }),
    edges: model.edges.map((edge) => {
      const routed = edge.route === "curve" ? routing.routes.get(edge.id) : undefined;
      return {
        id: edge.id,
        from: edge.from,
        to: edge.to,
        route: edge.route,
        points: routed?.points,
        label: routed?.label,
      };
    }),
  };
}

/**
 * The session graph: the spawn tree laid out by `layoutGraph`, the other edges
 * as curves over it. Solid creates and removes the elements; where they are is
 * written by the scene's frame loop, never through signals (spikes/graph-layout).
 */
export function GraphView(props: {
  graph: Graph;
  /** The session on screen: its agents are outlined. */
  openSessionId: string;
  mode: LayoutMode;
  selection: GraphSelection | undefined;
  onSelect: (selection: GraphSelection | undefined) => void;
  /** Hands out the scene, for the stress page's measurements. */
  onScene?: (scene: Scene) => void;
}) {
  const reducedMotion = createMediaQuery(REDUCED_MOTION_QUERY);
  // A plain mirror: the scene asks from effects and frames, where a signal must not be read.
  let calm = typeof matchMedia === "function" && matchMedia(REDUCED_MOTION_QUERY).matches;
  createEffect(reducedMotion, (reduced) => {
    calm = reduced;
  });
  const [pulses, setPulses] = createSignal<readonly string[]>([]);
  const scene = createScene({
    reducedMotion: () => calm,
    onPulses: setPulses,
  });
  const markerBase = createUniqueId();
  const model = createMemo(() => graphModel(props.graph, props.openSessionId));
  // The last layout's positions are what keeps siblings in the order they first appeared.
  const layout = createMemo<Layout>((previous) =>
    layoutGraph(props.graph.nodes, previous?.positions, props.mode),
  );
  // Fit starts the lanes over, packed; until then an edge keeps the lane it was given.
  const [packs, setPacks] = createSignal(0);
  const routing = createMemo<{ result: Routing; pack: number; mode: LayoutMode }>((previous) => {
    const current = layout();
    const pack = packs();
    const keep =
      previous && previous.pack === pack && previous.mode === current.mode
        ? previous.result.memory
        : undefined;
    const overlay = model().edges.filter((edge) => edge.route === "curve");
    return {
      result: routeOverlay(current.mode, boxesOf(current, model()), overlay, keep),
      pack,
      mode: current.mode,
    };
  });
  const outline = createMemo(() => describeGraph(model(), layout()));
  // The one tab stop among the nodes; arrows move it along the tree.
  const [focusId, setFocusId] = createSignal<string>();
  const tabStop = (): string | undefined => {
    const wanted = focusId();
    return wanted !== undefined && layout().positions.has(wanted) ? wanted : layout().order.at(0);
  };

  // The node whose edges are brought out: the one picked, else the one holding the focus.
  const [focused, setFocused] = createSignal<string>();
  const emphasis = createMemo(() => edgeEmphasis(model(), props.selection, focused()));
  const quiet = createMemo(
    () => model().edges.filter((edge) => edge.route === "curve").length > QUIET_ABOVE,
  );

  const nodeEls = new Map<string, SVGGElement>();
  const counts = new Map<string, number>();
  let seenGraph = false;
  let packed = 0;
  let canvas: SVGSVGElement | undefined;

  createEffect(
    () => ({ layout: layout(), model: model(), routing: routing() }),
    (next) => {
      scene.setLayout(sceneLayout(next.layout, next.model, next.routing.result), seenGraph);
      // A packing asked for by Fit: show the drawing whole again once the lanes have moved in.
      if (next.routing.pack !== packed) {
        packed = next.routing.pack;
        scene.fit();
      }
      // One pulse per edge whose count went up since the last graph; none for the first one.
      for (const edge of next.model.edges) {
        if (!edge.edge) continue;
        if (seenGraph && edge.count > (counts.get(edge.id) ?? 0)) scene.pulse(edge.id);
      }
      counts.clear();
      for (const edge of next.model.edges) counts.set(edge.id, edge.count);
      seenGraph = true;
    },
  );
  createEffect(
    () => props.selection,
    (selection) => scene.setAnchor(selection?.kind === "node" ? selection.id : undefined),
  );
  createEffect(tabStop, (id) => scene.pin(id));

  const [legend, setLegend] = createSignal(false);
  const legendId = createUniqueId();

  const pointers = new Map<number, { x: number; y: number }>();
  let dragged = false;
  let pinch: { distance: number } | undefined;
  const [dragging, setDragging] = createSignal(false);

  const local = (event: { clientX: number; clientY: number }): { x: number; y: number } => {
    const bounds = canvas?.getBoundingClientRect();
    return { x: event.clientX - (bounds?.left ?? 0), y: event.clientY - (bounds?.top ?? 0) };
  };

  function onPointerDown(event: PointerEvent): void {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pointers.size === 1) dragged = false;
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { distance: Math.hypot(a.x - b.x, a.y - b.y) };
      dragged = true;
    }
  }

  function onPointerMove(event: PointerEvent): void {
    const last = pointers.get(event.pointerId);
    if (!last) return;
    const dx = event.clientX - last.x;
    const dy = event.clientY - last.y;
    if (pointers.size === 1) {
      if (!dragged && Math.hypot(dx, dy) < DRAG_SLOP) return;
      if (!dragged) {
        dragged = true;
        setDragging(true);
        // From here the drag is the canvas's, wherever the pointer goes.
        canvas?.setPointerCapture?.(event.pointerId);
      }
      pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      scene.panBy(dx, dy);
      return;
    }
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pointers.size === 2 && pinch) {
      const [a, b] = [...pointers.values()];
      const distance = Math.hypot(a.x - b.x, a.y - b.y);
      const middle = local({ clientX: (a.x + b.x) / 2, clientY: (a.y + b.y) / 2 });
      // Half of this pointer's move is the move of the midpoint.
      scene.panBy(dx / 2, dy / 2);
      if (pinch.distance > 0 && distance > 0)
        scene.zoomAt(middle.x, middle.y, distance / pinch.distance);
      pinch.distance = distance;
    }
  }

  function onPointerEnd(event: PointerEvent): void {
    pointers.delete(event.pointerId);
    if (pointers.size < 2) pinch = undefined;
    if (pointers.size === 0) setDragging(false);
  }

  /** A click that ends a drag selects nothing. */
  function tapped(): boolean {
    const was = dragged;
    dragged = false;
    return !was;
  }

  function moveFocus(to: string | undefined): void {
    if (to === undefined) return;
    setFocusId(to);
    scene.pin(to);
    scene.reveal(to);
    nodeEls.get(to)?.focus({ preventScroll: true });
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const { order, parent, children } = layout();
    const target = event.target instanceof Element ? event.target.closest("[data-node]") : null;
    const at = target?.getAttribute("data-node") ?? undefined;
    const index = at === undefined ? -1 : order.indexOf(at);
    switch (event.key) {
      case "+":
      case "=":
        scene.zoomBy(ZOOM_STEP);
        break;
      case "-":
      case "_":
        scene.zoomBy(1 / ZOOM_STEP);
        break;
      case "0":
        fit();
        break;
      case "Escape":
        if (props.selection === undefined) return;
        props.onSelect(undefined);
        break;
      case "ArrowDown":
        if (index === -1) return;
        moveFocus(order.at(Math.min(order.length - 1, index + 1)));
        break;
      case "ArrowUp":
        if (index === -1) return;
        moveFocus(order.at(Math.max(0, index - 1)));
        break;
      case "ArrowLeft":
        if (at === undefined) return;
        moveFocus(parent.get(at));
        break;
      case "ArrowRight":
        if (at === undefined) return;
        moveFocus(children.get(at)?.at(0));
        break;
      case "Home":
        if (index === -1) return;
        moveFocus(order.at(0));
        break;
      case "End":
        if (index === -1) return;
        moveFocus(order.at(-1));
        break;
      case "Enter":
      case " ":
        if (at === undefined) return;
        props.onSelect({ kind: "node", id: at });
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  /** Shows the whole drawing, with the lanes packed again. */
  function fit(): void {
    setPacks((count) => count + 1);
    scene.fit();
  }

  onSettled(() => {
    props.onScene?.(scene);
    const element = canvas;
    if (!element) return () => scene.dispose();
    const measure = (): void => {
      const bounds = element.getBoundingClientRect();
      if (bounds.width > 0 && bounds.height > 0) {
        scene.setViewport({ w: bounds.width, h: bounds.height });
      }
    };
    measure();
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : undefined;
    observer?.observe(element);
    // Not passive: the wheel zooms the graph instead of scrolling the page.
    const onWheel = (event: WheelEvent): void => {
      event.preventDefault();
      const point = local(event);
      const scale = event.deltaMode === 1 ? 0.05 : 0.0015;
      scene.zoomAt(point.x, point.y, Math.exp(-event.deltaY * scale));
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      observer?.disconnect();
      element.removeEventListener("wheel", onWheel);
      scene.dispose();
    };
  });

  const selectedNode = (): string | undefined =>
    props.selection?.kind === "node" ? props.selection.id : undefined;
  const selectedEdge = (): string | undefined =>
    props.selection?.kind === "edge" ? props.selection.id : undefined;
  const hot = (edge: EdgeView): boolean => emphasis().hot.has(edge.id);
  const tone = (edge: EdgeView): MarkerTone => {
    if (edge.route === "tree" || hot(edge)) return "full";
    if (emphasis().active) return "faint";
    return quiet() ? "quiet" : "full";
  };
  const markerId = (kind: EdgeKind, which: MarkerTone): string =>
    which === "full" ? `${markerBase}-${kind}` : `${markerBase}-${kind}-${which}`;
  const marker = (edge: EdgeView): string | undefined =>
    edge.kind === "link" ? undefined : `url(#${markerId(edge.kind, tone(edge))})`;

  return (
    <div class="relative min-h-0 min-w-0 flex-1">
      <svg
        ref={(element) => (canvas = element)}
        class="graph-canvas"
        role="group"
        aria-label="Session graph"
        data-dragging={dragging() ? "" : undefined}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onKeyDown={onKeyDown}
        onClick={(event) => {
          // A tap on the background lets go of the selection.
          if (tapped() && event.target === event.currentTarget) props.onSelect(undefined);
        }}
      >
        <defs>
          <For each={markerSpecs}>
            {(spec) => {
              const { kind, which } = spec;
              return (
                <marker
                  id={markerId(kind, which)}
                  viewBox="0 0 8 8"
                  refX={arrowheads[kind].refX}
                  refY="4"
                  markerWidth={arrowheads[kind].size * markerScale[which]}
                  markerHeight={arrowheads[kind].size * markerScale[which]}
                  markerUnits="userSpaceOnUse"
                  orient="auto"
                >
                  <path
                    d={arrowheads[kind].d}
                    stroke-width="1.5"
                    stroke-linejoin="round"
                    stroke-linecap="round"
                    opacity={which === "full" ? undefined : markerTones[which]}
                    style={
                      arrowheads[kind].open
                        ? { fill: "none", stroke: arrowheads[kind].color }
                        : { fill: arrowheads[kind].color }
                    }
                  />
                </marker>
              );
            }}
          </For>
        </defs>
        <g ref={(element) => scene.mount(element)}>
          <g
            class="gedges"
            data-quiet={quiet() ? "" : undefined}
            data-focus={emphasis().active ? "" : undefined}
          >
            <For each={model().edges} keyed={(edge) => edge.id}>
              {(edge) => (
                <g
                  class="gedge"
                  data-edge={edge().id}
                  data-kind={edge().kind}
                  data-route={edge().route}
                  data-hot={hot(edge()) ? "" : undefined}
                  data-selected={selectedEdge() === edge().id ? "" : undefined}
                >
                  <path
                    class="gline"
                    ref={(element) => scene.edgeRef(edge().id, "path", element)}
                    marker-end={marker(edge())}
                  />
                  <Show when={edge().edge}>
                    <path
                      class="ghit"
                      ref={(element) => scene.edgeRef(edge().id, "hit", element)}
                      onClick={(event) => {
                        event.stopPropagation();
                        if (tapped()) props.onSelect({ kind: "edge", id: edge().id });
                      }}
                    >
                      <title>{edgeSentence(edge(), model())}</title>
                    </path>
                  </Show>
                  <Show when={edge().count > 1}>
                    <g class="gcount" ref={(element) => scene.edgeRef(edge().id, "label", element)}>
                      <rect x="-11" y="-8" width="22" height="16" rx="8" />
                      <text y="3.8">{edge().count > 99 ? "99+" : edge().count}</text>
                    </g>
                  </Show>
                </g>
              )}
            </For>
          </g>
          <g>
            <For each={model().nodes} keyed={(node) => node.id}>
              {(node) => (
                <g
                  ref={(element) => {
                    nodeEls.set(node().id, element);
                    scene.nodeRef(node().id, element);
                  }}
                  class="gnode"
                  data-node={node().id}
                  data-kind={node().node.kind}
                  data-own={node().own ? "" : undefined}
                  data-selected={selectedNode() === node().id ? "" : undefined}
                  role="button"
                  tabindex={tabStop() === node().id ? 0 : -1}
                  aria-label={nodeSentence(node())}
                  aria-pressed={selectedNode() === node().id ? "true" : "false"}
                  onFocus={() => {
                    setFocusId(node().id);
                    setFocused(node().id);
                  }}
                  onBlur={() => setFocused(undefined)}
                  onClick={(event) => {
                    event.stopPropagation();
                    if (tapped()) props.onSelect({ kind: "node", id: node().id });
                  }}
                >
                  <NodeShape node={node()} />
                </g>
              )}
            </For>
          </g>
          <g>
            <For each={pulses()} keyed={(id) => id}>
              {(id) => (
                <circle class="gpulse" r="4.5" ref={(element) => scene.pulseRef(id(), element)} />
              )}
            </For>
          </g>
        </g>
      </svg>
      <div class="absolute top-2 right-2 flex gap-1">
        <button
          type="button"
          class="btn bg-surface px-0 text-lg"
          aria-label="Zoom out"
          title="Zoom out (−)"
          onClick={() => scene.zoomBy(1 / ZOOM_STEP)}
        >
          <span aria-hidden="true">−</span>
        </button>
        <button
          type="button"
          class="btn bg-surface px-0 text-lg"
          aria-label="Zoom in"
          title="Zoom in (+)"
          onClick={() => scene.zoomBy(ZOOM_STEP)}
        >
          <span aria-hidden="true">+</span>
        </button>
        <button
          type="button"
          class="btn bg-surface px-2.5"
          title="Show the whole graph (0)"
          onClick={() => fit()}
        >
          Fit
        </button>
        <button
          type="button"
          class="btn bg-surface px-2.5"
          aria-expanded={legend() ? "true" : "false"}
          aria-controls={legendId}
          onClick={() => setLegend((open) => !open)}
        >
          Legend
        </button>
      </div>
      <Show when={legend()}>
        <div
          id={legendId}
          class="absolute top-14 right-2 z-10 max-w-[calc(100%-1rem)] rounded-md border border-line bg-surface p-2 text-xs shadow-md"
        >
          <ul aria-label="Legend" class="flex flex-col gap-1">
            <For each={edgeKinds}>
              {(kind) => (
                <li class="flex items-center gap-2">
                  <svg width="40" height="12" aria-hidden="true">
                    <g class="gedge" data-kind={kind}>
                      <path class="gline" d="M2,6H36" marker-end={`url(#${markerBase}-${kind})`} />
                    </g>
                  </svg>
                  {edgeKindWords(kind)}
                </li>
              )}
            </For>
            <li class="flex items-center gap-2">
              <svg width="40" height="14" aria-hidden="true">
                <rect
                  x="9"
                  y="1"
                  width="22"
                  height="12"
                  rx="3"
                  fill="none"
                  stroke="var(--accent)"
                  stroke-width="2"
                />
              </svg>
              Agent of the open session
            </li>
            <li class="max-w-56 text-muted">
              Pick a card to bring out its notes, reports and changed files.
            </li>
          </ul>
        </div>
      </Show>
      {/* The drawing, as text: who is under whom, the repos, and one sentence per edge. */}
      <div class="sr-only" data-part="graph-text">
        <h3>Session graph as text</h3>
        <Outline items={outline().tree} />
        <Show when={outline().repos.length > 0}>
          <h4>Repos</h4>
          <ul>
            <For each={outline().repos}>{(repo) => <li>{repo}</li>}</For>
          </ul>
        </Show>
        <Show when={outline().edges.length > 0}>
          <h4>Relations</h4>
          <ul>
            <For each={outline().edges}>{(edge) => <li>{edge}</li>}</For>
          </ul>
        </Show>
      </div>
    </div>
  );
}
