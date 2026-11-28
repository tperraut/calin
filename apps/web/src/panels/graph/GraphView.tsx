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
import {
  CanvasControls,
  createCalmScene,
  createTabStop,
  flag,
  createCanvasGestures,
  observeCanvas,
  onCanvasKey,
  type KeyCommand,
} from "./canvas";
import type { Scene, SceneLayout } from "./scene";

/** What the reader picked in the graph. */
export type GraphSelection = { kind: "node"; id: string } | { kind: "edge"; id: string };

const ZOOM_STEP = 1.3;

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
/** Side arrows along the tree: Left goes up to the parent, Right down to the first child. */
const alongTree =
  (parent: Layout["parent"], children: Layout["children"]) =>
  (key: string, at: string): KeyCommand | undefined => {
    if (key === "ArrowLeft") return { kind: "focus", id: parent.get(at) };
    if (key === "ArrowRight") return { kind: "focus", id: children.get(at)?.at(0) };
    return undefined;
  };

type Routed = { result: Routing; pack: number; mode: LayoutMode };

/** The lanes the edges keep: the last routing's, unless Fit asked for a packing or the mode changed. */
const laneMemory = (previous: Routed | undefined, pack: number, mode: LayoutMode) =>
  previous && previous.pack === pack && previous.mode === mode ? previous.result.memory : undefined;

/**
 * The edges whose count went up since `before`, the counts of the last graph:
 * one pulse each. None for the first graph, and none for a link that is no edge.
 */
function grownEdges(
  edges: readonly EdgeView[],
  before: ReadonlyMap<string, number> | undefined,
): string[] {
  if (before === undefined) return [];
  return edges
    .filter((edge) => edge.edge && edge.count > (before.get(edge.id) ?? 0))
    .map((edge) => edge.id);
}

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
  const [pulses, setPulses] = createSignal<readonly string[]>([]);
  const scene = createCalmScene(setPulses);
  const markerBase = createUniqueId();
  const model = createMemo(() => graphModel(props.graph, props.openSessionId));
  // The last layout's positions are what keeps siblings in the order they first appeared.
  const layout = createMemo<Layout>((previous) =>
    layoutGraph(props.graph.nodes, previous?.positions, props.mode),
  );
  // Fit starts the lanes over, packed; until then an edge keeps the lane it was given.
  const [packs, setPacks] = createSignal(0);
  const routing = createMemo<Routed>((previous) => {
    const current = layout();
    const pack = packs();
    const keep = laneMemory(previous, pack, current.mode);
    const overlay = model().edges.filter((edge) => edge.route === "curve");
    return {
      result: routeOverlay(current.mode, boxesOf(current, model()), overlay, keep),
      pack,
      mode: current.mode,
    };
  });
  const outline = createMemo(() => describeGraph(model(), layout()));
  // The node whose edges are brought out: the one picked, else the one holding the focus.
  const [focused, setFocused] = createSignal<string>();
  const emphasis = createMemo(() => edgeEmphasis(model(), props.selection, focused()));
  const quiet = createMemo(
    () => model().edges.filter((edge) => edge.route === "curve").length > QUIET_ABOVE,
  );

  let counts: ReadonlyMap<string, number> | undefined;
  let packed = 0;
  let canvas: SVGSVGElement | undefined;

  createEffect(
    () => ({ layout: layout(), model: model(), routing: routing() }),
    (next) => {
      scene.setLayout(
        sceneLayout(next.layout, next.model, next.routing.result),
        counts !== undefined,
      );
      // A packing asked for by Fit: show the drawing whole again once the lanes have moved in.
      if (next.routing.pack !== packed) {
        packed = next.routing.pack;
        scene.fit();
      }
      for (const id of grownEdges(next.model.edges, counts)) scene.pulse(id);
      counts = new Map(next.model.edges.map((edge) => [edge.id, edge.count]));
    },
  );
  createEffect(
    () => props.selection,
    (selection) => scene.setAnchor(picked(selection, "node")),
  );
  // The one tab stop among the nodes; arrows move it along the tree.
  const tabStop = createTabStop(scene, () => ({
    order: layout().order,
    has: (id) => layout().positions.has(id),
  }));

  const gestures = createCanvasGestures(scene, () => canvas);

  function onKeyDown(event: KeyboardEvent): void {
    const { order, parent, children } = layout();
    onCanvasKey(
      event,
      {
        order,
        selected: props.selection !== undefined,
        step: ZOOM_STEP,
        side: alongTree(parent, children),
      },
      {
        zoomBy: (factor) => scene.zoomBy(factor),
        fit,
        select: (id) => props.onSelect(id === undefined ? undefined : { kind: "node", id }),
        focus: tabStop.move,
      },
    );
  }

  /** Shows the whole drawing, with the lanes packed again. */
  function fit(): void {
    setPacks((count) => count + 1);
    scene.fit();
  }

  onSettled(() => {
    props.onScene?.(scene);
    return observeCanvas(canvas, scene, (size) => scene.setViewport(size), gestures.onWheel);
  });

  const selectedNode = (): string | undefined => picked(props.selection, "node");
  const hot = (edge: EdgeView): boolean => emphasis().hot.has(edge.id);
  const marker = (edge: EdgeView): string | undefined =>
    markerUrl(markerBase, edge, markerTone(edge, hot(edge), emphasis().active, quiet()));

  return (
    <div class="relative min-h-0 min-w-0 flex-1">
      <svg
        ref={(element) => (canvas = element)}
        class="graph-canvas"
        role="group"
        aria-label="Session graph"
        data-dragging={flag(gestures.dragging())}
        onPointerDown={gestures.onPointerDown}
        onPointerMove={gestures.onPointerMove}
        onPointerUp={gestures.onPointerEnd}
        onPointerCancel={gestures.onPointerEnd}
        onKeyDown={onKeyDown}
        onClick={(event) => {
          // A tap on the background lets go of the selection.
          if (gestures.tappedBackground(event)) props.onSelect(undefined);
        }}
      >
        <GraphMarkers base={markerBase} />
        <g ref={(element) => scene.mount(element)}>
          <g class="gedges" data-quiet={flag(quiet())} data-focus={flag(emphasis().active)}>
            <For each={model().edges} keyed={(edge) => edge.id}>
              {(edge) => (
                <GraphEdge
                  edge={edge()}
                  model={model()}
                  hot={hot(edge())}
                  selected={picked(props.selection, "edge") === edge().id}
                  marker={marker(edge())}
                  scene={scene}
                  onTap={() => {
                    if (gestures.tapped()) props.onSelect({ kind: "edge", id: edge().id });
                  }}
                />
              )}
            </For>
          </g>
          <g>
            <For each={model().nodes} keyed={(node) => node.id}>
              {(node) => (
                <GraphNode
                  node={node()}
                  selected={selectedNode() === node().id}
                  tabStop={tabStop.stop() === node().id}
                  onElement={(element) => {
                    tabStop.element(node().id, element);
                    scene.nodeRef(node().id, element);
                  }}
                  onFocus={() => {
                    tabStop.focused(node().id);
                    setFocused(node().id);
                  }}
                  onBlur={() => setFocused(undefined)}
                  onTap={() => {
                    if (gestures.tapped()) props.onSelect({ kind: "node", id: node().id });
                  }}
                />
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
      <CanvasControls
        step={ZOOM_STEP}
        zoomBy={(factor) => scene.zoomBy(factor)}
        fit={fit}
        fitTitle="Show the whole graph (0)"
      >
        <GraphLegend markerBase={markerBase} />
      </CanvasControls>
      <GraphText outline={outline()} />
    </div>
  );
}

/** What the reader picked, when it is of `kind`. */
const picked = (
  selection: GraphSelection | undefined,
  kind: GraphSelection["kind"],
): string | undefined => (selection?.kind === kind ? selection.id : undefined);

/** The id of the arrowhead of `kind` in tone `which`, under `base`. */
const markerIdOf = (base: string, kind: EdgeKind, which: MarkerTone): string =>
  which === "full" ? `${base}-${kind}` : `${base}-${kind}-${which}`;

/** The arrowhead at the end of an edge; a link has none. */
const markerUrl = (base: string, edge: EdgeView, which: MarkerTone): string | undefined =>
  edge.kind === "link" ? undefined : `url(#${markerIdOf(base, edge.kind, which)})`;

/**
 * How strongly an edge's arrowhead shows: in full on the tree and on an edge
 * brought out; faint while a card is picked; quieter when there are many.
 */
function markerTone(edge: EdgeView, hot: boolean, picking: boolean, quiet: boolean): MarkerTone {
  if (edge.route === "tree" || hot) return "full";
  if (picking) return "faint";
  return quiet ? "quiet" : "full";
}

/** Every arrowhead, by kind and tone. */
function GraphMarkers(props: { base: string }) {
  return (
    <defs>
      <For each={markerSpecs}>
        {(spec) => {
          const { kind, which } = spec;
          return (
            <marker
              id={markerIdOf(props.base, kind, which)}
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
  );
}

/** One edge: its line, the wider line that takes the taps, and its count past one. */
function GraphEdge(props: {
  edge: EdgeView;
  model: GraphModel;
  hot: boolean;
  selected: boolean;
  marker: string | undefined;
  scene: Scene;
  onTap: () => void;
}) {
  return (
    <g
      class="gedge"
      data-edge={props.edge.id}
      data-kind={props.edge.kind}
      data-route={props.edge.route}
      data-hot={flag(props.hot)}
      data-selected={flag(props.selected)}
    >
      <path
        class="gline"
        ref={(element) => props.scene.edgeRef(props.edge.id, "path", element)}
        marker-end={props.marker}
      />
      <Show when={props.edge.edge}>
        <path
          class="ghit"
          ref={(element) => props.scene.edgeRef(props.edge.id, "hit", element)}
          onClick={(event) => {
            event.stopPropagation();
            props.onTap();
          }}
        >
          <title>{edgeSentence(props.edge, props.model)}</title>
        </path>
      </Show>
      <Show when={props.edge.count > 1}>
        <g class="gcount" ref={(element) => props.scene.edgeRef(props.edge.id, "label", element)}>
          <rect x="-11" y="-8" width="22" height="16" rx="8" />
          <text y="3.8">{props.edge.count > 99 ? "99+" : props.edge.count}</text>
        </g>
      </Show>
    </g>
  );
}

/** One node, a button: Enter or a tap selects it. */
function GraphNode(props: {
  node: NodeView;
  selected: boolean;
  /** True for the one node the Tab key reaches. */
  tabStop: boolean;
  onElement: (element: SVGGElement) => void;
  onFocus: () => void;
  onBlur: () => void;
  onTap: () => void;
}) {
  return (
    <g
      ref={(element) => props.onElement(element)}
      class="gnode"
      data-node={props.node.id}
      data-kind={props.node.node.kind}
      data-own={flag(props.node.own)}
      data-selected={flag(props.selected)}
      role="button"
      tabindex={props.tabStop ? 0 : -1}
      aria-label={nodeSentence(props.node)}
      aria-pressed={props.selected ? "true" : "false"}
      onFocus={() => props.onFocus()}
      onBlur={() => props.onBlur()}
      onClick={(event) => {
        event.stopPropagation();
        props.onTap();
      }}
    >
      <NodeShape node={props.node} />
    </g>
  );
}

/** What the arrowheads and outlines mean. */
function GraphLegend(props: { markerBase: string }) {
  return (
    <>
      <For each={edgeKinds}>
        {(kind) => (
          <li class="flex items-center gap-2">
            <svg width="40" height="12" aria-hidden="true">
              <g class="gedge" data-kind={kind}>
                <path class="gline" d="M2,6H36" marker-end={`url(#${props.markerBase}-${kind})`} />
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
    </>
  );
}

/** The drawing, as text: who is under whom, the repos, and one sentence per edge. */
function GraphText(props: { outline: ReturnType<typeof describeGraph> }) {
  return (
    <div class="sr-only" data-part="graph-text">
      <h3>Session graph as text</h3>
      <Outline items={props.outline.tree} />
      <Show when={props.outline.repos.length > 0}>
        <h4>Repos</h4>
        <ul>
          <For each={props.outline.repos}>{(repo) => <li>{repo}</li>}</For>
        </ul>
      </Show>
      <Show when={props.outline.edges.length > 0}>
        <h4>Relations</h4>
        <ul>
          <For each={props.outline.edges}>{(edge) => <li>{edge}</li>}</For>
        </ul>
      </Show>
    </div>
  );
}
