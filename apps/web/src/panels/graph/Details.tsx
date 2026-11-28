import { For, Show, type ParentProps } from "solid-js";
import type { JSX } from "@solidjs/web";
import { SessionStatusBadge } from "../../components/Badge";
import type { Agent } from "../../gen/Agent";
import type { GraphNode } from "../../gen/GraphNode";
import type { SessionStatus } from "../../gen/SessionStatus";
import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import {
  absoluteTime,
  clockTime,
  compactCount,
  filesLabel,
  formatCost,
  formatDuration,
  relativeTime,
} from "../../lib/format";
import { agentLook, repoModeLabel } from "../../lib/status";
import { sessionPath } from "../registry";
import type { GraphSelection } from "./GraphView";
import {
  edgeKindWords,
  edgeSentence,
  edgesOf,
  repoChangers,
  sessionLead,
  type EdgeView,
  type GraphModel,
  type NodeView,
} from "./model";
import {
  arrowKindWords,
  spanDuration,
  spanKindLabel,
  spanStatus,
  type LaneView,
} from "./timelineModel";

/**
 * Where the details of what was picked show: a sheet over the bottom of the
 * drawing on a phone, a column beside it when the pane is wide.
 */
export function DetailsSheet(
  props: ParentProps<{ title: string; wide: boolean; onClose: () => void }>,
) {
  return (
    <section
      aria-label="Details"
      class={[
        "flex flex-col gap-2 overflow-y-auto overscroll-contain bg-surface p-3 text-sm",
        props.wide
          ? "w-72 shrink-0 border-l border-line"
          : "absolute inset-x-0 bottom-0 z-20 max-h-[60%] rounded-t-lg border-t border-line shadow-lg",
      ]}
    >
      <div class="flex items-start justify-between gap-2">
        <h2 class="min-w-0 pt-2.5 font-semibold wrap-anywhere">{props.title}</h2>
        <button
          type="button"
          class="btn btn-ghost shrink-0 px-0"
          aria-label="Close details"
          title="Close details"
          onClick={() => props.onClose()}
        >
          <span aria-hidden="true">✕</span>
        </button>
      </div>
      {props.children}
    </section>
  );
}

function Facts(props: { rows: readonly (readonly [string, JSX.Element | undefined])[] }) {
  return (
    <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
      <For each={props.rows.filter((row) => row[1] !== undefined && row[1] !== "")}>
        {(row) => (
          <>
            <dt class="text-muted">{row[0]}</dt>
            <dd class="min-w-0 wrap-anywhere">{row[1]}</dd>
          </>
        )}
      </For>
    </dl>
  );
}

function Relations(props: {
  edges: readonly EdgeView[];
  model: GraphModel;
  onSelect: (selection: GraphSelection) => void;
}) {
  return (
    <Show when={props.edges.length > 0}>
      <div>
        <h3 class="text-xs font-medium text-muted">Relations</h3>
        <ul aria-label="Relations" class="flex flex-col">
          <For each={props.edges} keyed={(edge) => edge.id}>
            {(edge) => (
              <li>
                <button
                  type="button"
                  class="flex min-h-11 w-full items-center text-left underline decoration-line underline-offset-4 hover:decoration-current"
                  onClick={() => props.onSelect({ kind: "edge", id: edge().id })}
                >
                  {edgeSentence(edge(), props.model)}
                </button>
              </li>
            )}
          </For>
        </ul>
      </div>
    </Show>
  );
}

/** A session's status as its badge; nothing when the session has none. */
const statusBadge = (status: SessionStatus | null | undefined): JSX.Element | undefined =>
  status ? <SessionStatusBadge status={status} /> : undefined;

function AgentFacts(props: { agent: Agent; lead: GraphNode | undefined }) {
  return (
    <Facts
      rows={[
        ["Role", props.agent.role],
        ["Status", agentLook(props.agent.status).label],
        ["Runner", <span class="font-mono">{props.agent.runner}</span>],
        ["Model", props.agent.model ?? undefined],
        ["Cost", formatCost(props.agent.cost_usd)],
        [
          "Tokens",
          `${compactCount(props.agent.tokens_in)} in · ${compactCount(props.agent.tokens_out)} out`,
        ],
        ["Session", props.lead?.session_title ?? undefined],
        ["Session status", statusBadge(props.lead?.session_status)],
      ]}
    />
  );
}

function AgentDetails(props: {
  node: NodeView;
  model: GraphModel;
  openSessionId: string;
  onSelect: (selection: GraphSelection) => void;
  onShowAgent: (agentId: string) => void;
}) {
  const agent = () => props.node.node.agent;
  return (
    <>
      <Show when={agent()}>
        {(known) => (
          <AgentFacts agent={known()} lead={sessionLead(props.node.node.session_id, props.model)} />
        )}
      </Show>
      <div class="flex flex-wrap gap-2">
        <Show when={props.node.node.session_id}>
          {(sessionId) => (
            <a class="btn" href={sessionPath(sessionId())}>
              Open session
            </a>
          )}
        </Show>
        <Show when={props.node.own && agent()}>
          {(known) => (
            <button type="button" class="btn" onClick={() => props.onShowAgent(known().id)}>
              Show in transcript
            </button>
          )}
        </Show>
      </div>
      <Relations
        edges={edgesOf(props.node.id, props.model)}
        model={props.model}
        onSelect={props.onSelect}
      />
    </>
  );
}

function RepoDetails(props: {
  node: NodeView;
  model: GraphModel;
  onSelect: (selection: GraphSelection) => void;
  onOpenChanges: () => void;
}) {
  const changers = () => repoChangers(props.node.id, props.model);
  return (
    <>
      <Facts
        rows={[
          ["Repo", props.node.title],
          [
            "Mode",
            props.node.node.repo_mode === null
              ? undefined
              : repoModeLabel(props.node.node.repo_mode),
          ],
        ]}
      />
      <div>
        <h3 class="text-xs font-medium text-muted">Changed by</h3>
        <Show
          when={changers().length > 0}
          fallback={<p class="text-muted">Nobody changed it yet.</p>}
        >
          <ul aria-label="Changed by">
            <For each={changers()} keyed={(one) => one.edgeId}>
              {(one) => (
                <li>
                  <button
                    type="button"
                    class="flex min-h-11 w-full items-center justify-between gap-2 text-left"
                    onClick={() => props.onSelect({ kind: "edge", id: one().edgeId })}
                  >
                    <span class="min-w-0 underline decoration-line underline-offset-4 wrap-anywhere">
                      {one().name}
                    </span>
                    <span class="shrink-0 text-muted">{filesLabel(one().count)}</span>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
      <div>
        <button type="button" class="btn" onClick={() => props.onOpenChanges()}>
          Open changes
        </button>
      </div>
    </>
  );
}

function EdgeDetails(props: { edge: EdgeView; model: GraphModel; now: number }) {
  return (
    <>
      <p>{edgeSentence(props.edge, props.model)}</p>
      <Facts
        rows={[
          ["Kind", edgeKindWords(props.edge.kind)],
          ["Count", String(props.edge.count)],
          [
            "Last",
            props.edge.edge
              ? `${absoluteTime(props.edge.edge.last_ts)} (${relativeTime(props.edge.edge.last_ts, props.now)})`
              : undefined,
          ],
          ["Latest", props.edge.edge?.label ?? undefined],
        ]}
      />
    </>
  );
}

/** The details of the node or edge picked in the graph. Nothing when it is no longer in the graph. */
export function GraphDetails(props: {
  model: GraphModel;
  selection: GraphSelection | undefined;
  openSessionId: string;
  wide: boolean;
  now: number;
  onSelect: (selection: GraphSelection | undefined) => void;
  onShowAgent: (agentId: string) => void;
  onOpenChanges: () => void;
}) {
  const node = () =>
    props.selection?.kind === "node" ? props.model.nodeById.get(props.selection.id) : undefined;
  const edge = () =>
    props.selection?.kind === "edge" ? props.model.edgeById.get(props.selection.id) : undefined;
  const close = (): void => props.onSelect(undefined);
  return (
    <>
      <Show when={node()}>
        {(picked) => (
          <DetailsSheet title={picked().title} wide={props.wide} onClose={close}>
            <Show
              when={picked().node.kind === "repo"}
              fallback={
                <Show
                  when={picked().node.kind === "agent"}
                  fallback={
                    <>
                      <p class="text-muted">You: where this session's requests come from.</p>
                      <Relations
                        edges={edgesOf(picked().id, props.model)}
                        model={props.model}
                        onSelect={props.onSelect}
                      />
                    </>
                  }
                >
                  <AgentDetails
                    node={picked()}
                    model={props.model}
                    openSessionId={props.openSessionId}
                    onSelect={props.onSelect}
                    onShowAgent={props.onShowAgent}
                  />
                </Show>
              }
            >
              <RepoDetails
                node={picked()}
                model={props.model}
                onSelect={props.onSelect}
                onOpenChanges={props.onOpenChanges}
              />
            </Show>
          </DetailsSheet>
        )}
      </Show>
      <Show when={edge()}>
        {(picked) => (
          <DetailsSheet title={edgeKindWords(picked().kind)} wide={props.wide} onClose={close}>
            <EdgeDetails edge={picked()} model={props.model} now={props.now} />
          </DetailsSheet>
        )}
      </Show>
    </>
  );
}

/** The details of a span picked in the timeline. */
export function SpanDetails(props: {
  span: TimelineSpan;
  lane: LaneView;
  now: number;
  wide: boolean;
  /** Set when the span is a tool call the transcript on screen holds. */
  onShowInTranscript: (() => void) | undefined;
  onClose: () => void;
}) {
  return (
    <DetailsSheet
      title={props.span.title || spanKindLabel(props.span.kind)}
      wide={props.wide}
      onClose={props.onClose}
    >
      <Facts
        rows={[
          ["Kind", spanKindLabel(props.span.kind)],
          ["Tool", props.span.tool_kind ?? undefined],
          ["Agent", props.lane.label],
          ["Start", `${clockTime(props.span.start)} · ${absoluteTime(props.span.start)}`],
          [
            "Duration",
            props.span.end === null
              ? `${formatDuration(spanDuration(props.span, props.now))} so far`
              : formatDuration(spanDuration(props.span, props.now)),
          ],
          ["Status", spanStatus(props.span)],
        ]}
      />
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

/** The details of an arrow picked in the timeline. */
export function ArrowDetails(props: {
  arrow: TimelineArrow;
  from: string;
  to: string;
  wide: boolean;
  onClose: () => void;
}) {
  return (
    <DetailsSheet
      title={arrowKindWords(props.arrow.kind)}
      wide={props.wide}
      onClose={props.onClose}
    >
      <Facts
        rows={[
          ["Kind", arrowKindWords(props.arrow.kind)],
          ["From", props.from],
          ["To", props.to],
          ["Time", `${clockTime(props.arrow.ts)} · ${absoluteTime(props.arrow.ts)}`],
          ["Label", props.arrow.label ?? undefined],
        ]}
      />
    </DetailsSheet>
  );
}
