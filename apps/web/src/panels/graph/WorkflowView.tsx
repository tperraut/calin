import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  onSettled,
  Show,
} from "solid-js";
import type { WorkflowRun } from "../../gen/WorkflowRun";
import { createMediaQuery } from "../../lib/media";
import { dollars, kindIcon, kindWord, runStatusWords } from "../../lib/workflow";
import { DetailsSheet } from "./Details";
import { clip } from "./model";
import { REDUCED_MOTION_QUERY } from "./GraphView";
import {
  alongOrder,
  CanvasControls,
  createCanvasGestures,
  observeCanvas,
  onCanvasKey,
} from "./canvas";
import { createScene } from "./scene";
import { layoutWorkflow, type Orientation, type WorkflowLayout } from "./workflowLayout";
import {
  nodeLines,
  nodeName,
  portSentence,
  portTargets,
  runOverlay,
  stateWords,
  takenPath,
  waitsForYou,
  type NodeOverlay,
  type RunOverlay,
} from "./workflowModel";

const ZOOM_STEP = 1.3;

/** Kinds whose work shows in the transcript, so a node of theirs can be found there. */
const IN_TRANSCRIPT = new Set(["agent", "check", "gate", "ask"]);

/** The card of one node: kind as icon and word, id, label; its result on the first line's right. */
function NodeCard(props: { node: NodeOverlay; layout: WorkflowLayout }) {
  const size = () => props.layout.byId.get(props.node.id) ?? { w: 168, h: 60 };
  /** What the node's corner says: running, its loop count, or its last port and how often it ran. */
  const corner = (): string => {
    const node = props.node;
    if (node.state === "active") return waitsForYou(node) ? "Waiting for you" : "Running";
    if (node.loop !== undefined) return node.loop;
    if (node.state !== "finished") return "";
    const port =
      node.lastPort === null ? "" : `${node.failed ? "✕" : "→"} ${clip(node.lastPort, 9)}`;
    return node.runs > 1 ? `${port} ×${node.runs}` : port;
  };
  return (
    <>
      <rect class="wfocus" x="-5" y="-5" width={size().w + 10} height={size().h + 10} rx="12" />
      <rect class="wshape" width={size().w} height={size().h} rx="8" />
      <text class="wkind" x="10" y="17">
        {kindIcon(props.node.info.kind)} {kindWord(props.node.info.kind)}
      </text>
      <text class="wcorner" x={size().w - 10} y="17" text-anchor="end">
        {corner()}
      </text>
      <text class="wtitle" x="10" y="35">
        {clip(props.node.id, 20)}
      </text>
      <text class="wlabel" x="10" y="51">
        {clip(props.node.info.label, 24)}
      </text>
    </>
  );
}

/** One fact of the details, skipped when it has nothing to say. */
function Fact(props: { term: string; value: string | null | undefined; mono?: boolean }) {
  return (
    <Show when={props.value !== null && props.value !== undefined && props.value !== ""}>
      <dt class="text-muted">{props.term}</dt>
      <dd
        class={["min-w-0 whitespace-pre-wrap wrap-anywhere", { "font-mono text-xs": props.mono }]}
      >
        {props.value}
      </dd>
    </Show>
  );
}

/** The details of the node picked in the drawing. */
export function WorkflowNodeDetails(props: {
  run: WorkflowRun;
  node: NodeOverlay;
  wide: boolean;
  onSelect: (id: string | undefined) => void;
  /** Absent when the transcript holds no row of this node, or the kind leaves none. */
  onShowInTranscript: (() => void) | undefined;
}) {
  const ports = () => portTargets(props.run, props.node.id);
  return (
    <DetailsSheet title={props.node.id} wide={props.wide} onClose={() => props.onSelect(undefined)}>
      <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        <Fact
          term="Kind"
          value={`${kindIcon(props.node.info.kind)} ${kindWord(props.node.info.kind)}`}
        />
        <Fact term="Label" value={props.node.info.label} />
        <Fact term="State" value={stateWords(props.node)} />
        <Fact term="Runs" value={String(props.node.runs)} />
        <Fact term="Loop" value={props.node.loop} />
        <Fact term="Last port" value={props.node.lastPort} />
        <Fact term="Outcome" value={props.node.outcome} mono={true} />
      </dl>
      <Show when={ports().length > 0}>
        <div>
          <h3 class="text-xs font-medium text-muted">Ports</h3>
          <ul aria-label="Ports" class="flex flex-col">
            <For each={ports()} keyed={(one) => one.port}>
              {(target) => (
                <li data-failure={target().failure ? "" : undefined}>
                  <Show
                    when={target().to}
                    fallback={<p class="flex min-h-11 items-center">{portSentence(target())}</p>}
                  >
                    {(to) => (
                      <button
                        type="button"
                        class="flex min-h-11 w-full items-center text-left underline decoration-line underline-offset-4 hover:decoration-current"
                        onClick={() => props.onSelect(to())}
                      >
                        {portSentence(target())}
                      </button>
                    )}
                  </Show>
                </li>
              )}
            </For>
          </ul>
        </div>
      </Show>
      <Show when={props.onShowInTranscript}>
        {(show) => (
          <div>
            <button type="button" class="btn" onClick={() => show()()}>
              Show in transcript
            </button>
          </div>
        )}
      </Show>
    </DetailsSheet>
  );
}

/** The run's name, its status in words, why it failed, and what it cost against its budget. */
export function WorkflowHeader(props: { run: WorkflowRun }) {
  const share = (): number => {
    const budget = props.run.budget_usd;
    return budget === null || budget <= 0 ? 0 : Math.min(1, props.run.cost_usd / budget);
  };
  return (
    <div
      data-part="workflow-header"
      class="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line bg-surface px-3 py-1.5 text-sm"
    >
      <span class="min-w-0 font-medium wrap-anywhere">{props.run.name}</span>
      <span data-status={props.run.status} class="wstatus font-medium">
        {runStatusWords(props.run.status)}
      </span>
      <Show when={props.run.status === "failed" && props.run.reason}>
        {(reason) => <span class="min-w-0 text-muted wrap-anywhere">{reason()}</span>}
      </Show>
      <Show
        when={props.run.budget_usd}
        fallback={<span class="text-muted">spent {dollars(props.run.cost_usd)}</span>}
      >
        {(budget) => (
          <span class="ml-auto flex items-center gap-2">
            <span>
              spent {dollars(props.run.cost_usd)} of {dollars(budget())}
            </span>
            <span aria-hidden="true" class="h-1.5 w-16 overflow-hidden rounded-full bg-sunken">
              <span
                class={["block h-full", share() >= 1 ? "bg-tone-danger" : "bg-accent"]}
                style={{ width: `${Math.round(share() * 100)}%` }}
              />
            </span>
          </span>
        )}
      </Show>
    </div>
  );
}

/**
 * The workflow of a session as a picture: its graph laid out by
 * `layoutWorkflow`, the run over it. Pan and zoom are the session graph's own
 * (its scene), so are the keyboard keys; the graph never changes during a run,
 * so nothing here moves but the view.
 */
export function WorkflowView(props: {
  run: WorkflowRun;
  orientation: Orientation;
  selection: string | undefined;
  onSelect: (id: string | undefined) => void;
}) {
  const reducedMotion = createMediaQuery(REDUCED_MOTION_QUERY);
  let calm = typeof matchMedia === "function" && matchMedia(REDUCED_MOTION_QUERY).matches;
  createEffect(reducedMotion, (reduced) => {
    calm = reduced;
  });
  const scene = createScene({ reducedMotion: () => calm, onPulses: () => {} });
  const markerBase = createUniqueId();
  // The graph is fixed for a run: only the orientation lays it out again.
  const layout = createMemo(() => layoutWorkflow(props.run.graph, props.orientation));
  const overlay = createMemo((): RunOverlay => runOverlay(props.run));
  const [focusId, setFocusId] = createSignal<string>();
  const tabStop = (): string | undefined => {
    const wanted = focusId();
    return wanted !== undefined && layout().byId.has(wanted) ? wanted : layout().order.at(0);
  };
  const nodeEls = new Map<string, SVGGElement>();
  let canvas: SVGSVGElement | undefined;
  let measured = false;

  createEffect(layout, (next) => {
    scene.setLayout(
      {
        mode: "columns",
        nodes: next.nodes.map((node) => ({
          id: node.id,
          x: node.x,
          y: node.y,
          w: node.w,
          h: node.h,
        })),
        edges: [],
        width: next.width,
        height: next.height,
      },
      false,
    );
    // A new orientation is a new drawing: show it whole.
    if (measured) scene.fit(true);
  });
  createEffect(tabStop, (id) => scene.pin(id));

  const gestures = createCanvasGestures(scene, () => canvas);

  function moveFocus(to: string | undefined): void {
    if (to === undefined) return;
    setFocusId(to);
    scene.pin(to);
    scene.reveal(to);
    nodeEls.get(to)?.focus({ preventScroll: true });
  }

  function onKeyDown(event: KeyboardEvent): void {
    const { order } = layout();
    onCanvasKey(
      event,
      // The run reads in one direction: the side arrows step through it too.
      { order, selected: props.selection !== undefined, step: ZOOM_STEP, side: alongOrder(order) },
      {
        zoomBy: (factor) => scene.zoomBy(factor),
        fit: () => scene.fit(),
        select: (id) => props.onSelect(id),
        focus: moveFocus,
      },
    );
  }

  onSettled(() => {
    const element = canvas;
    if (!element) return () => scene.dispose();
    const stop = observeCanvas(
      element,
      (size) => {
        scene.setViewport(size);
        // Fit on open: the whole run at a glance.
        if (!measured) scene.fit(true);
        measured = true;
      },
      gestures.onWheel,
    );
    return () => {
      stop();
      scene.dispose();
    };
  });

  const edgeOf = (index: number) => overlay().edges[index];
  const marker = (index: number): string => {
    const edge = edgeOf(index);
    const kind = edge && edge.taken > 0 ? "taken" : "idle";
    return `url(#${markerBase}-${kind})`;
  };

  return (
    <div class="relative min-h-0 min-w-0 flex-1">
      <svg
        ref={(element) => (canvas = element)}
        class="graph-canvas"
        role="group"
        aria-label="Workflow graph"
        data-orientation={props.orientation}
        data-dragging={gestures.dragging() ? "" : undefined}
        onPointerDown={gestures.onPointerDown}
        onPointerMove={gestures.onPointerMove}
        onPointerUp={gestures.onPointerEnd}
        onPointerCancel={gestures.onPointerEnd}
        onKeyDown={onKeyDown}
        onClick={(event) => {
          if (gestures.tapped() && event.target === event.currentTarget) props.onSelect(undefined);
        }}
      >
        <defs>
          <For each={["taken", "idle"] as const}>
            {(kind) => (
              <marker
                id={`${markerBase}-${kind}`}
                viewBox="0 0 8 8"
                refX="7"
                refY="4"
                markerWidth={kind === "taken" ? 9 : 7}
                markerHeight={kind === "taken" ? 9 : 7}
                markerUnits="userSpaceOnUse"
                orient="auto"
              >
                <path
                  d="M0,0.5L8,4L0,7.5z"
                  style={{ fill: kind === "taken" ? "var(--fg)" : "var(--muted)" }}
                />
              </marker>
            )}
          </For>
        </defs>
        <g ref={(element) => scene.mount(element)}>
          <g>
            <For each={layout().edges} keyed={(edge) => `${layout().orientation}:${edge.index}`}>
              {(placed) => (
                <g
                  class="wedge"
                  data-edge={placed().index}
                  data-back={placed().back ? "" : undefined}
                  data-failure={edgeOf(placed().index)?.failure ? "" : undefined}
                  data-taken={(edgeOf(placed().index)?.taken ?? 0) > 0 ? "" : undefined}
                >
                  <path class="wline" d={placed().d} marker-end={marker(placed().index)} />
                  <text
                    class="wport"
                    x={placed().label.x}
                    y={placed().label.y}
                    text-anchor={placed().label.anchor}
                  >
                    {edgeOf(placed().index)?.failure
                      ? `✕ ${placed().edge.port}`
                      : placed().edge.port}
                  </text>
                  <Show when={(edgeOf(placed().index)?.taken ?? 0) > 1}>
                    <g
                      class="wcount"
                      transform={`translate(${placed().middle.x},${placed().middle.y})`}
                    >
                      <rect x="-13" y="-8" width="26" height="16" rx="8" />
                      <text y="3.8">×{edgeOf(placed().index)?.taken}</text>
                    </g>
                  </Show>
                </g>
              )}
            </For>
          </g>
          <g>
            <For each={overlay().nodes} keyed={(node) => node.id}>
              {(node) => (
                <g
                  ref={(element) => {
                    nodeEls.set(node().id, element);
                    scene.nodeRef(node().id, element);
                  }}
                  class="wnode"
                  data-node={node().id}
                  data-kind={node().info.kind}
                  data-state={node().state}
                  data-failed={node().failed ? "" : undefined}
                  data-selected={props.selection === node().id ? "" : undefined}
                  role="button"
                  tabindex={tabStop() === node().id ? 0 : -1}
                  aria-label={nodeName(node())}
                  aria-pressed={props.selection === node().id ? "true" : "false"}
                  onFocus={() => setFocusId(node().id)}
                  onClick={(event) => {
                    event.stopPropagation();
                    if (gestures.tapped()) props.onSelect(node().id);
                  }}
                >
                  <NodeCard node={node()} layout={layout()} />
                </g>
              )}
            </For>
          </g>
        </g>
      </svg>
      <CanvasControls
        step={ZOOM_STEP}
        zoomBy={(factor) => scene.zoomBy(factor)}
        fit={() => scene.fit()}
        fitTitle="Show the whole workflow (0)"
      >
        <For
          each={
            [
              ["active", "Running now: thick outline, “Running”"],
              ["finished", "Finished: “→ port” it left through, “×2” when it ran twice"],
              ["failed", "Left through a failure port: “✕ port”"],
              ["idle", "Not run yet: faint"],
            ] as const
          }
        >
          {([state, words]) => (
            <li class="flex items-center gap-2">
              <svg width="40" height="16" aria-hidden="true">
                <g
                  class="wnode"
                  data-state={state === "failed" ? "finished" : state}
                  data-failed={state === "failed" ? "" : undefined}
                >
                  <rect class="wshape" x="4" y="2" width="32" height="12" rx="3" />
                </g>
              </svg>
              {words}
            </li>
          )}
        </For>
        <For
          each={
            [
              ["taken", "Taken: bold, “×3” when taken more than once"],
              ["idle", "Never taken: faint"],
              ["failure", "Failure port: dashed, “✕” before its name"],
            ] as const
          }
        >
          {([kind, words]) => (
            <li class="flex items-center gap-2">
              <svg width="40" height="12" aria-hidden="true">
                <g
                  class="wedge"
                  data-taken={kind === "taken" ? "" : undefined}
                  data-failure={kind === "failure" ? "" : undefined}
                >
                  <path class="wline" d="M2,6H36" />
                </g>
              </svg>
              {words}
            </li>
          )}
        </For>
      </CanvasControls>
    </div>
  );
}

/** The run as text, for those who cannot see the drawing: every node, then the path taken. */
export function WorkflowText(props: { run: WorkflowRun }) {
  const overlay = createMemo(() => runOverlay(props.run));
  const path = createMemo(() => takenPath(props.run));
  return (
    <div class="sr-only" data-part="workflow-text">
      <h3>Workflow {props.run.name} as text</h3>
      <ol>
        <For each={nodeLines(props.run, overlay())}>{(line) => <li>{line}</li>}</For>
      </ol>
      <p>
        {path().length === 0 ? "Path taken: nothing yet." : `Path taken: ${path().join(" → ")}.`}
      </p>
    </div>
  );
}

/** Whether the transcript keeps rows for nodes of this kind (see "Show in transcript"). */
export const inTranscript = (kind: string): boolean => IN_TRANSCRIPT.has(kind);

/** Orientation for a pane `width` pixels wide; see `ACROSS_MIN_WIDTH`. */
export { ACROSS_MIN_WIDTH } from "./workflowLayout";
export type { Orientation } from "./workflowLayout";
