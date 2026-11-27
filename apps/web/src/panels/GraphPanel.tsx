import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
} from "solid-js";
import { createWidth } from "../lib/media";
import { treeRevision } from "../state/sessionTree";
import { createOrchestrationData, type OrchestrationView } from "./graph/data";
import { ArrowDetails, GraphDetails, SpanDetails } from "./graph/Details";
import "./graph/graph.css";
import { GraphView, type GraphSelection } from "./graph/GraphView";
import { graphModel } from "./graph/model";
import { Scrubber } from "./graph/Scrubber";
import { laneViews } from "./graph/timelineModel";
import { TimelineView, type TimelineSelection } from "./graph/TimelineView";
import type { PanelProps } from "./registry";

/** From this width the tree is drawn in columns and the details sit beside it. */
const WIDE_PANE = 720;
const views: readonly { id: OrchestrationView; label: string }[] = [
  { id: "graph", label: "Graph" },
  { id: "timeline", label: "Timeline" },
];

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

/**
 * The orchestration views of the session's whole tree: who spawned whom, who
 * told what to whom and who changed which repo, as a graph; every agent's
 * activity, as a timeline. Both are read again, a moment after, whenever the
 * open session records an event or any session of its tree changes.
 */
export default function GraphPanel(props: PanelProps) {
  const store = props.store;
  const data = createOrchestrationData(store.orchestration);
  const [view, setView] = createSignal<OrchestrationView>("graph");
  const [selection, setSelection] = createSignal<GraphSelection>();
  const [picked, setPicked] = createSignal<TimelineSelection>();
  const [now, setNow] = createSignal(Date.now());
  let root: HTMLDivElement | undefined;
  const width = createWidth(() => root);
  const wide = (): boolean => width() >= WIDE_PANE;

  // Nothing is read behind another panel or in a background tab; what was missed is read on the way back.
  const [tabVisible, setTabVisible] = createSignal(document.visibilityState === "visible");
  onSettled(() => {
    const onVisibility = (): void => {
      setTabVisible(document.visibilityState === "visible");
    };
    document.addEventListener("visibilitychange", onVisibility);
    // Open spans grow with the clock.
    const clock = setInterval(() => {
      if (props.active && document.visibilityState === "visible") setNow(Date.now());
    }, 1000);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      clearInterval(clock);
      data.dispose();
    };
  });
  createEffect(
    () => props.active && tabVisible(),
    (visible) => data.setVisible(visible),
  );
  createEffect(view, (shown) => data.setView(shown));
  // A memo: an effect runs again whenever its sources do, even for an equal value,
  // and a session outside the tree must not cost a read.
  const revision = createMemo(() => `${store.view().seq}/${treeRevision(store.tree())}`);
  createEffect(revision, () => data.changed(), { defer: true });

  const model = createMemo(() => {
    const graph = data.graph();
    return graph ? graphModel(graph, store.id) : undefined;
  });
  const titleOf = (sessionId: string): string | undefined =>
    store.tree().sessions.find((session) => session.id === sessionId)?.title;
  const lanes = createMemo(() => {
    const timeline = data.timeline();
    return timeline ? laneViews(timeline, store.id, titleOf) : [];
  });
  const pickedSpan = createMemo(() => {
    const chosen = picked();
    if (chosen?.kind !== "span") return undefined;
    const lane = lanes().find((one) => one.id === chosen.agentId);
    const span = lane?.lane.spans.find((one) => one.id === chosen.id);
    return lane && span ? { lane, span } : undefined;
  });
  const pickedArrow = createMemo(() => {
    const chosen = picked();
    if (chosen?.kind !== "arrow") return undefined;
    return data.timeline()?.arrows.find((arrow) => arrow.id === chosen.id);
  });
  const laneName = (agentId: string | null): string =>
    agentId === null ? "You" : (lanes().find((lane) => lane.id === agentId)?.label ?? "An agent");
  /** A tool span of the open session whose item the transcript holds can be shown there. */
  const inTranscript = (): (() => void) | undefined => {
    const chosen = pickedSpan();
    if (!chosen || chosen.span.kind !== "tool" || chosen.lane.lane.session_id !== store.id) {
      return undefined;
    }
    const itemId = chosen.span.id;
    const held = store
      .view()
      .transcript.some((entry) => entry.kind === "item" && entry.item.id === itemId);
    return held ? () => props.links.showItem(itemId) : undefined;
  };

  return (
    <div ref={(element) => (root = element)} class="orch flex min-h-0 min-w-0 flex-1 flex-col">
      <div class="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-2 py-1">
        <div role="group" aria-label="View" class="flex gap-1">
          <For each={views}>
            {(one) => (
              <button
                type="button"
                class="btn px-3"
                aria-pressed={view() === one.id ? "true" : "false"}
                onClick={() => setView(one.id)}
              >
                {one.label}
              </button>
            )}
          </For>
        </div>
        <Show when={data.refreshing()}>
          <span class="appear-late text-xs text-muted" role="status">
            Updating…
          </span>
        </Show>
      </div>
      <Switch>
        <Match when={view() === "graph"}>
          {/* A failed read keeps the last graph on screen, under what went wrong. */}
          <Show when={data.graph() && data.graphError()}>
            {(message) => (
              <p
                role="alert"
                class="flex shrink-0 items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-1 text-sm"
              >
                <span class="min-w-0">The graph could not be updated: {message()}</span>
                <button type="button" class="btn" onClick={() => data.retry()}>
                  Try again
                </button>
              </p>
            )}
          </Show>
          <Show
            when={data.graph()}
            fallback={
              <Show
                when={data.graphError()}
                fallback={<p class="p-3 text-sm text-muted">Loading the graph…</p>}
              >
                {(message) => (
                  <Problem
                    what="The graph could not be loaded"
                    message={message()}
                    onRetry={() => data.retry()}
                  />
                )}
              </Show>
            }
          >
            {(graph) => (
              <>
                <div class="relative flex min-h-0 flex-1">
                  <Show
                    when={graph().nodes.length > 0}
                    fallback={
                      <p class="flex-1 p-6 text-center text-sm text-muted">
                        {data.live()
                          ? "Nothing to draw yet: the graph fills in as agents start."
                          : "Nothing had happened yet at this moment."}
                      </p>
                    }
                  >
                    <GraphView
                      graph={graph()}
                      openSessionId={store.id}
                      mode={wide() ? "columns" : "indented"}
                      selection={selection()}
                      onSelect={setSelection}
                    />
                  </Show>
                  <Show when={model()}>
                    {(known) => (
                      <GraphDetails
                        model={known()}
                        selection={selection()}
                        openSessionId={store.id}
                        wide={wide()}
                        now={now()}
                        onSelect={setSelection}
                        onShowAgent={(agentId) => props.links.showAgent(agentId)}
                        onOpenChanges={() => props.links.openPanel("changes")}
                      />
                    )}
                  </Show>
                </div>
                <Scrubber
                  from={graph().from_ts}
                  to={graph().to_ts}
                  at={data.at()}
                  live={data.live()}
                  playing={data.playing()}
                  onScrub={(at) => data.scrub(at)}
                  onLive={(live) => data.setLive(live)}
                  onPlay={() => data.play()}
                  onPause={() => data.pause()}
                />
              </>
            )}
          </Show>
        </Match>
        <Match when={view() === "timeline"}>
          <Show when={data.timeline() && data.timelineError()}>
            {(message) => (
              <p
                role="alert"
                class="flex shrink-0 items-center justify-between gap-2 border-b border-line bg-attention-soft px-3 py-1 text-sm"
              >
                <span class="min-w-0">The timeline could not be updated: {message()}</span>
                <button type="button" class="btn" onClick={() => data.retry()}>
                  Try again
                </button>
              </p>
            )}
          </Show>
          <Show
            when={data.timeline()}
            fallback={
              <Show
                when={data.timelineError()}
                fallback={<p class="p-3 text-sm text-muted">Loading the timeline…</p>}
              >
                {(message) => (
                  <Problem
                    what="The timeline could not be loaded"
                    message={message()}
                    onRetry={() => data.retry()}
                  />
                )}
              </Show>
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
                    openSessionId={store.id}
                    titleOf={titleOf}
                    now={now()}
                    selection={picked()}
                    onSelect={setPicked}
                  />
                </Show>
                <Show when={pickedSpan()}>
                  {(chosen) => (
                    <SpanDetails
                      span={chosen().span}
                      lane={chosen().lane}
                      now={now()}
                      wide={wide()}
                      onShowInTranscript={inTranscript()}
                      onClose={() => setPicked(undefined)}
                    />
                  )}
                </Show>
                <Show when={pickedArrow()}>
                  {(arrow) => (
                    <ArrowDetails
                      arrow={arrow()}
                      from={laneName(arrow().from_agent_id)}
                      to={laneName(arrow().to_agent_id)}
                      wide={wide()}
                      onClose={() => setPicked(undefined)}
                    />
                  )}
                </Show>
              </div>
            )}
          </Show>
        </Match>
      </Switch>
    </div>
  );
}
