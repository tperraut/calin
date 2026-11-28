import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  type Accessor,
} from "solid-js";
import { createWidth } from "../lib/media";
import type { SessionStore } from "../state/sessionStore";
import { treeRevision } from "../state/sessionTree";
import { createOrchestrationData, type OrchestrationData } from "./graph/data";
import { ArrowDetails, GraphDetails, SpanDetails } from "./graph/Details";
import "./graph/graph.css";
import { GraphView, type GraphSelection } from "./graph/GraphView";
import { graphModel } from "./graph/model";
import {
  laneName,
  nodeRowKey,
  nowRunning,
  orientationFor,
  pickedArrow,
  pickedSpan,
  shownView,
  spanItem,
  viewChoices,
  type PanelView,
} from "./graph/panelModel";
import { Scrubber } from "./graph/Scrubber";
import { laneViews } from "./graph/timelineModel";
import { TimelineView, type TimelineSelection } from "./graph/TimelineView";
import { runOverlay } from "./graph/workflowModel";
import {
  inTranscript as keepsRows,
  WorkflowHeader,
  WorkflowNodeDetails,
  WorkflowText,
  WorkflowView,
  type Orientation,
} from "./graph/WorkflowView";
import type { PanelLinks, PanelProps } from "./registry";

/** From this width the tree is drawn in columns and the details sit beside it. */
const WIDE_PANE = 720;

function Problem(props: { what: string; message: string; onRetry: () => void }) {
  return (
    <div
      role="alert"
      class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center"
    >
      <p class="text-sm">
        {props.what}: {props.message}
      </p>
      <button type="button" class="btn" onClick={() => props.onRetry()}>
        Try again
      </button>
    </div>
  );
}

/** Before the first answer: that it is on its way, or why it failed. */
function Pending(props: {
  failure: string | undefined;
  loading: string;
  what: string;
  onRetry: () => void;
}) {
  return (
    <Show when={props.failure} fallback={<p class="p-3 text-sm text-muted">{props.loading}</p>}>
      {(message) => <Problem what={props.what} message={message()} onRetry={props.onRetry} />}
    </Show>
  );
}

/** A failed read over what is still on screen from the last one. */
function StaleNotice(props: { what: string; message: string; onRetry: () => void }) {
  return (
    <p
      role="alert"
      class="flex shrink-0 items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-1 text-sm"
    >
      <span class="min-w-0">
        {props.what}: {props.message}
      </span>
      <button type="button" class="btn" onClick={() => props.onRetry()}>
        Try again
      </button>
    </p>
  );
}

function ViewToggle(props: {
  hasWorkflow: boolean;
  view: PanelView;
  onPick: (view: PanelView) => void;
}) {
  return (
    <div role="group" aria-label="View" class="flex gap-1">
      <For each={viewChoices(props.hasWorkflow)}>
        {(one) => (
          <button
            type="button"
            class="btn px-3"
            aria-pressed={props.view === one.id ? "true" : "false"}
            onClick={() => props.onPick(one.id)}
          >
            {one.label}
          </button>
        )}
      </For>
    </div>
  );
}

function WorkflowPane(props: {
  store: SessionStore;
  links: PanelLinks;
  orientation: Orientation;
  wide: boolean;
  picked: string | undefined;
  onPick: (nodeId: string | undefined) => void;
}) {
  const run = () => props.store.workflow.run();
  const pickedOverlay = createMemo(() => {
    const id = props.picked;
    const known = run();
    if (id === undefined || !known) return undefined;
    return runOverlay(known).byId.get(id);
  });
  /** The newest transcript row of a node, for the kinds the transcript keeps rows of. */
  const showNode = (): (() => void) | undefined => {
    const node = pickedOverlay();
    if (!node || !keepsRows(node.info.kind)) return undefined;
    const key = nodeRowKey(props.store.view().transcript, node.id);
    return key === undefined ? undefined : () => props.links.showItem(key);
  };
  // Announced once each time another node starts running.
  const announced = createMemo(() => nowRunning(run()));
  return (
    <>
      <p class="sr-only" role="status" aria-live="polite" data-part="now-running">
        {announced()}
      </p>
      <Show
        when={run()}
        fallback={
          <Pending
            failure={props.store.workflow.failure()}
            loading="Loading the workflow…"
            what="The workflow could not be loaded"
            onRetry={() => props.store.workflow.now()}
          />
        }
      >
        {(known) => (
          <>
            <WorkflowHeader run={known()} />
            <div class="relative flex min-h-0 flex-1">
              <WorkflowView
                run={known()}
                orientation={props.orientation}
                selection={props.picked}
                onSelect={props.onPick}
              />
              <Show when={pickedOverlay()}>
                {(node) => (
                  <WorkflowNodeDetails
                    run={known()}
                    node={node()}
                    wide={props.wide}
                    onSelect={props.onPick}
                    onShowInTranscript={showNode()}
                  />
                )}
              </Show>
            </div>
            <WorkflowText run={known()} />
          </>
        )}
      </Show>
    </>
  );
}

function GraphPane(props: {
  data: OrchestrationData;
  openSessionId: string;
  links: PanelLinks;
  wide: boolean;
  now: number;
  selection: GraphSelection | undefined;
  onSelect: (selection: GraphSelection | undefined) => void;
}) {
  const model = createMemo(() => {
    const graph = props.data.graph();
    return graph ? graphModel(graph, props.openSessionId) : undefined;
  });
  return (
    <>
      {/* A failed read keeps the last graph on screen, under what went wrong. */}
      <Show when={props.data.graph() && props.data.graphError()}>
        {(message) => (
          <StaleNotice
            what="The graph could not be updated"
            message={message()}
            onRetry={() => props.data.retry()}
          />
        )}
      </Show>
      <Show
        when={props.data.graph()}
        fallback={
          <Pending
            failure={props.data.graphError()}
            loading="Loading the graph…"
            what="The graph could not be loaded"
            onRetry={() => props.data.retry()}
          />
        }
      >
        {(graph) => (
          <>
            <div class="relative flex min-h-0 flex-1">
              <Show
                when={graph().nodes.length > 0}
                fallback={
                  <p class="flex-1 p-6 text-center text-sm text-muted">
                    {props.data.live()
                      ? "Nothing to draw yet: the graph fills in as agents start."
                      : "Nothing had happened yet at this moment."}
                  </p>
                }
              >
                <GraphView
                  graph={graph()}
                  openSessionId={props.openSessionId}
                  mode={props.wide ? "columns" : "indented"}
                  selection={props.selection}
                  onSelect={props.onSelect}
                />
              </Show>
              <Show when={model()}>
                {(known) => (
                  <GraphDetails
                    model={known()}
                    selection={props.selection}
                    openSessionId={props.openSessionId}
                    wide={props.wide}
                    now={props.now}
                    onSelect={props.onSelect}
                    onShowAgent={(agentId) => props.links.showAgent(agentId)}
                    onOpenChanges={() => props.links.openPanel("changes")}
                  />
                )}
              </Show>
            </div>
            <Scrubber
              from={graph().from_ts}
              to={graph().to_ts}
              at={props.data.at()}
              live={props.data.live()}
              playing={props.data.playing()}
              onScrub={(at) => props.data.scrub(at)}
              onLive={(live) => props.data.setLive(live)}
              onPlay={() => props.data.play()}
              onPause={() => props.data.pause()}
            />
          </>
        )}
      </Show>
    </>
  );
}

function TimelinePane(props: {
  data: OrchestrationData;
  store: SessionStore;
  links: PanelLinks;
  wide: boolean;
  now: number;
  picked: TimelineSelection | undefined;
  onPick: (selection: TimelineSelection | undefined) => void;
}) {
  const titleOf = (sessionId: string): string | undefined =>
    props.store.tree().sessions.find((session) => session.id === sessionId)?.title;
  const lanes = createMemo(() => {
    const timeline = props.data.timeline();
    return timeline ? laneViews(timeline, props.store.id, titleOf) : [];
  });
  const span = createMemo(() => pickedSpan(lanes(), props.picked));
  const arrow = createMemo(() => pickedArrow(props.data.timeline(), props.picked));
  /** A tool span of the open session whose item the transcript holds can be shown there. */
  const inTranscript = (): (() => void) | undefined => {
    const itemId = spanItem(span(), props.store.id, props.store.view().transcript);
    return itemId === undefined ? undefined : () => props.links.showItem(itemId);
  };
  const close = (): void => props.onPick(undefined);
  return (
    <>
      <Show when={props.data.timeline() && props.data.timelineError()}>
        {(message) => (
          <StaleNotice
            what="The timeline could not be updated"
            message={message()}
            onRetry={() => props.data.retry()}
          />
        )}
      </Show>
      <Show
        when={props.data.timeline()}
        fallback={
          <Pending
            failure={props.data.timelineError()}
            loading="Loading the timeline…"
            what="The timeline could not be loaded"
            onRetry={() => props.data.retry()}
          />
        }
      >
        {(timeline) => (
          <div class="relative flex min-h-0 flex-1">
            <Show
              when={timeline().lanes.length > 0}
              fallback={
                <p class="flex-1 p-6 text-center text-sm text-muted">
                  Nothing to draw yet: a lane appears when an agent starts.
                </p>
              }
            >
              <TimelineView
                timeline={timeline()}
                openSessionId={props.store.id}
                titleOf={titleOf}
                now={props.now}
                selection={props.picked}
                onSelect={props.onPick}
              />
            </Show>
            <Show when={span()}>
              {(chosen) => (
                <SpanDetails
                  span={chosen().span}
                  lane={chosen().lane}
                  now={props.now}
                  wide={props.wide}
                  onShowInTranscript={inTranscript()}
                  onClose={close}
                />
              )}
            </Show>
            <Show when={arrow()}>
              {(picked) => (
                <ArrowDetails
                  arrow={picked()}
                  from={laneName(lanes(), picked().from_agent_id)}
                  to={laneName(lanes(), picked().to_agent_id)}
                  wide={props.wide}
                  onClose={close}
                />
              )}
            </Show>
          </div>
        )}
      </Show>
    </>
  );
}

/** Whether the tab is in the foreground. */
function createTabVisible(): Accessor<boolean> {
  const [visible, setVisible] = createSignal(document.visibilityState === "visible");
  onSettled(() => {
    const onVisibility = (): void => {
      setVisible(document.visibilityState === "visible");
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  });
  return visible;
}

/** The time, moved on every second while the panel shows: open spans grow with the clock. */
function createClock(active: () => boolean): Accessor<number> {
  const [now, setNow] = createSignal(Date.now());
  onSettled(() => {
    const clock = setInterval(() => {
      if (active() && document.visibilityState === "visible") setNow(Date.now());
    }, 1000);
    return () => clearInterval(clock);
  });
  return now;
}

/**
 * The orchestration views of the session's whole tree: who spawned whom, who
 * told what to whom and who changed which repo, as a graph; every agent's
 * activity, as a timeline. Both are read again, a moment after, whenever the
 * open session records an event or any session of its tree changes.
 */
export default function GraphPanel(props: PanelProps) {
  const store = props.store;
  const data = createOrchestrationData(store.orchestration);
  // What the reader chose; until they do, a session that runs a workflow opens on it.
  const [chosenView, setView] = createSignal<PanelView>();
  const view = (): PanelView => shownView(chosenView(), store.workflow.known());
  const [pickedNode, setPickedNode] = createSignal<string>();
  const [selection, setSelection] = createSignal<GraphSelection>();
  const [picked, setPicked] = createSignal<TimelineSelection>();
  let root: HTMLDivElement | undefined;
  const width = createWidth(() => root);
  const wide = (): boolean => width() >= WIDE_PANE;
  // Nothing is read behind another panel or in a background tab; what was missed is read on the way back.
  const tabVisible = createTabVisible();
  const now = createClock(() => props.active);
  onSettled(() => () => data.dispose());
  createEffect(
    () => ({ visible: props.active && tabVisible(), view: view() }),
    ({ visible, view: shown }) => {
      // The workflow view reads its own route; the orchestration reads wait meanwhile.
      data.setVisible(visible && shown !== "workflow");
      store.workflow.setVisible(visible && shown === "workflow");
    },
  );
  onSettled(() => () => store.workflow.setVisible(false));
  createEffect(view, (shown) => {
    if (shown !== "workflow") data.setView(shown);
  });
  // A memo: an effect runs again whenever its sources do, even for an equal value,
  // and a session outside the tree must not cost a read.
  const revision = createMemo(() => `${store.view().seq}/${treeRevision(store.tree())}`);
  createEffect(revision, () => data.changed(), { defer: true });

  return (
    <div ref={(element) => (root = element)} class="orch flex min-h-0 min-w-0 flex-1 flex-col">
      <div class="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-2 py-1">
        <ViewToggle hasWorkflow={store.workflow.known()} view={view()} onPick={setView} />
        <Show when={data.refreshing()}>
          <span class="appear-late text-xs text-muted" role="status">
            Updating…
          </span>
        </Show>
      </div>
      <Switch>
        <Match when={view() === "workflow"}>
          <WorkflowPane
            store={store}
            links={props.links}
            orientation={orientationFor(width())}
            wide={wide()}
            picked={pickedNode()}
            onPick={setPickedNode}
          />
        </Match>
        <Match when={view() === "graph"}>
          <GraphPane
            data={data}
            openSessionId={store.id}
            links={props.links}
            wide={wide()}
            now={now()}
            selection={selection()}
            onSelect={setSelection}
          />
        </Match>
        <Match when={view() === "timeline"}>
          <TimelinePane
            data={data}
            store={store}
            links={props.links}
            wide={wide()}
            now={now()}
            picked={picked()}
            onPick={setPicked}
          />
        </Match>
      </Switch>
    </div>
  );
}
