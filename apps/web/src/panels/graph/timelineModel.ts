import type { EdgeKind } from "../../gen/EdgeKind";
import type { SpanKind } from "../../gen/SpanKind";
import type { Timeline } from "../../gen/Timeline";
import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineLane } from "../../gen/TimelineLane";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import type { ToolKind } from "../../gen/ToolKind";
import { clockTime, formatDuration } from "../../lib/format";
import { itemLook } from "../../lib/status";
import { clipMiddle } from "./model";

/** Height of one lane, and of the axis above the first one. */
export const LANE_HEIGHT = 44;
export const AXIS_HEIGHT = 28;
/** A turn's bar within its lane, and a tool's bar within the turn. */
export const TURN_BAR = { y: 6, h: 32 } as const;
export const TOOL_BAR = { y: 10, h: 10 } as const;
/** Narrower than this a tool span gets no label: text is never cut by its own mark. */
export const LABEL_MIN_WIDTH = 40;
const LABEL_CHAR_WIDTH = 5.6;
/** The smallest width anything is drawn with, so an instant is still a mark. */
const MIN_WIDTH = 2;

/** A lane as the timeline draws and names it. */
export type LaneView = {
  readonly id: string;
  readonly lane: TimelineLane;
  /** Depth in the spawn tree: 0 for a root session's lead. */
  readonly depth: number;
  /** The agent's role; a lead is known by its session's title when it is given. */
  readonly label: string;
  /**
   * The label as the narrow lane column shows it: sibling sessions share their
   * start, so the title is cut in the middle and keeps the end that tells them apart.
   */
  readonly short: string;
  /** True for the agents of the session on screen. */
  readonly own: boolean;
};

const agentNode = (agentId: string): string => `agent:${agentId}`;

/** The lanes in the server's order, each with its depth and label. Pure. */
export function laneViews(
  timeline: Timeline,
  openSessionId: string,
  titleOf: (sessionId: string) => string | undefined,
): LaneView[] {
  const byNode = new Map(timeline.lanes.map((lane) => [agentNode(lane.agent.id), lane]));
  const depthOf = (lane: TimelineLane, seen: Set<string>): number => {
    const parent = lane.parent_id === null ? undefined : byNode.get(lane.parent_id);
    // The user is not a lane; a cycle must not hang the page.
    if (!parent || seen.has(lane.agent.id)) return 0;
    seen.add(lane.agent.id);
    return 1 + depthOf(parent, seen);
  };
  return timeline.lanes.map((lane) => {
    const lead = lane.agent.kind !== "subagent";
    const title = lead ? titleOf(lane.session_id) : undefined;
    return {
      id: lane.agent.id,
      lane,
      depth: depthOf(lane, new Set()),
      label: title === undefined ? lane.agent.role : `${lane.agent.role} · ${title}`,
      short:
        title === undefined ? lane.agent.role : `${lane.agent.role} · ${clipMiddle(title, 22)}`,
      own: lane.session_id === openSessionId,
    };
  });
}

/** Where time sits on the axis: `from` is at x = 0, one millisecond is `pxPerMs` pixels. */
export type TimeScale = { readonly from: number; readonly pxPerMs: number };

export const timeToX = (scale: TimeScale, ts: number): number => (ts - scale.from) * scale.pxPerMs;
export const xToTime = (scale: TimeScale, x: number): number => scale.from + x / scale.pxPerMs;

/** The scale at which `from`..`end` fills `width` pixels. */
export function fitScale(from: number, end: number, width: number): TimeScale {
  const span = Math.max(1000, end - from);
  return { from, pxPerMs: Math.max(1, width) / span };
}

/** The end of the axis: the newest event, or now while something is still open. */
export function timelineEnd(timeline: Timeline, now: number): number {
  const open = timeline.lanes.some((lane) => lane.spans.some((span) => span.end === null));
  return open ? Math.max(timeline.to_ts, now) : timeline.to_ts;
}

export type SpanBox = { readonly x: number; readonly w: number; readonly open: boolean };

/** A span on the axis. One with no end is still open: it reaches `now`, and grows with it. */
export function spanBox(span: TimelineSpan, scale: TimeScale, now: number): SpanBox {
  const open = span.end === null;
  const end = span.end ?? Math.max(now, span.start);
  const x = timeToX(scale, span.start);
  return { x, w: Math.max(MIN_WIDTH, timeToX(scale, end) - x), open };
}

/** The spans that cross the pixels `x0`..`x1` of the axis: the others are not drawn. */
export function visibleSpans(
  spans: readonly TimelineSpan[],
  scale: TimeScale,
  now: number,
  x0: number,
  x1: number,
): TimelineSpan[] {
  return spans.filter((span) => {
    const box = spanBox(span, scale, now);
    return box.x <= x1 && box.x + box.w >= x0;
  });
}

/** Spans this far outside the viewport are still drawn, so a short scroll shows no gap. */
const CULL_MARGIN = 240;

/** The pixels of the axis whose spans and arrows are drawn: the viewport and a margin each side. */
export type CullRange = { readonly x0: number; readonly x1: number };

export const cullRange = (scrollX: number, room: number): CullRange => ({
  x0: scrollX - CULL_MARGIN,
  x1: scrollX + room + CULL_MARGIN,
});

/** A span as the chart holds it: its lane's row and its box on the axis. */
export type DrawnSpan = {
  readonly key: string;
  readonly agentId: string;
  readonly row: number;
  readonly span: TimelineSpan;
  readonly box: SpanBox;
};

/** A span's id on the chart: span ids are only unique within a lane. */
export const spanKey = (agentId: string, spanId: string): string => `${agentId}/${spanId}`;

// Turns under what lies inside them.
const drawOrder: Record<string, number> = { turn: 0, waiting: 1, tool: 2 };

/** The spans of every lane within `range`, turns first so that they lie under the rest. */
export function drawnSpans(
  lanes: readonly LaneView[],
  scale: TimeScale,
  now: number,
  range: CullRange,
): DrawnSpan[] {
  const list = lanes.flatMap((view, row) =>
    visibleSpans(view.lane.spans, scale, now, range.x0, range.x1).map((span) => ({
      key: spanKey(view.id, span.id),
      agentId: view.id,
      row,
      span,
      box: spanBox(span, scale, now),
    })),
  );
  return list.sort((a, b) => (drawOrder[a.span.kind] ?? 3) - (drawOrder[b.span.kind] ?? 3));
}

/** The arrows within `range`, in the server's order; one naming an agent with no lane is not drawn. */
export function drawnArrows(
  arrows: readonly TimelineArrow[],
  laneIndex: ReadonlyMap<string, number>,
  scale: TimeScale,
  range: CullRange,
): { arrow: TimelineArrow; box: ArrowBox }[] {
  return arrows.flatMap((arrow) => {
    const box = arrowBox(arrow, laneIndex, scale);
    return box && box.x >= range.x0 && box.x <= range.x1 ? [{ arrow, box }] : [];
  });
}

/** The key of the first span of the first lane that has one: the tab stop until a span takes the focus. */
export function firstKey(lanes: readonly LaneView[]): string | undefined {
  const lane = lanes.find((view) => view.lane.spans.length > 0);
  return lane && spanKey(lane.id, lane.lane.spans[0].id);
}

/** Who an agent is, in sentences: the user has no lane. */
export const laneName = (lanes: readonly LaneView[], agentId: string | null): string =>
  agentId === null ? "you" : (lanes.find((lane) => lane.id === agentId)?.label ?? "an agent");

/** A turn with the tool and waiting spans that lie inside it (graph.rs). */
export type TurnGroup = { readonly turn: TimelineSpan | undefined; readonly inner: TimelineSpan[] };

/** The spans of a lane, each tool or waiting span under the turn it lies in. */
export function nestSpans(spans: readonly TimelineSpan[]): TurnGroup[] {
  const groups: TurnGroup[] = [];
  const turns = spans.filter((span) => span.kind === "turn");
  const byTurn = new Map<string, TurnGroup>();
  for (const turn of turns) {
    const group = { turn, inner: [] };
    byTurn.set(turn.id, group);
    groups.push(group);
  }
  const loose: TimelineSpan[] = [];
  for (const span of spans) {
    if (span.kind === "turn") continue;
    const turn = turns.findLast(
      (one) => one.start <= span.start && (one.end === null || span.start <= one.end),
    );
    const group = turn ? byTurn.get(turn.id) : undefined;
    if (group) group.inner.push(span);
    else loose.push(span);
  }
  // A span outside every turn is still shown, after the turns.
  if (loose.length > 0) groups.push({ turn: undefined, inner: loose });
  return groups;
}

/** Tool kinds folded into the few colors that stay distinguishable; the label and the details name the kind itself. */
export type Series = "read" | "write" | "run" | "task" | "think" | "other";

const seriesOf: Record<ToolKind, Series> = {
  read: "read",
  search: "read",
  fetch: "read",
  edit: "write",
  delete: "write",
  move: "write",
  execute: "run",
  task: "task",
  think: "think",
  other: "other",
};

export const toolSeries = (kind: ToolKind | null): Series =>
  kind === null ? "other" : (seriesOf[kind] ?? "other");

/** In legend order; never cycled or reassigned. */
export const seriesLegend: readonly { series: Series; label: string }[] = [
  { series: "read", label: "Read, search, fetch" },
  { series: "write", label: "Edit, delete, move" },
  { series: "run", label: "Run" },
  { series: "task", label: "Delegate" },
  { series: "think", label: "Think" },
  { series: "other", label: "Other" },
];

/** The label of a tool span `width` pixels wide: its title cut to fit, or none when it would not. */
export function spanLabel(title: string, width: number): string | undefined {
  if (width < LABEL_MIN_WIDTH) return undefined;
  const room = Math.floor((width - 6) / LABEL_CHAR_WIDTH);
  if (title.length <= room) return title;
  return room < 4 ? undefined : `${title.slice(0, room - 1).trimEnd()}…`;
}

const arrowWords: Partial<Record<EdgeKind, string>> = {
  spawned: "Spawned subagent",
  dispatched: "Dispatched child session",
  message: "Note",
  report: "Report",
};

export const arrowKindWords = (kind: EdgeKind): string => arrowWords[kind] ?? kind;

/** The shape that marks where an arrow lands, one per kind. */
export type ArrowMarker = "triangle" | "circle" | "diamond" | "square";

const markers: Partial<Record<EdgeKind, ArrowMarker>> = {
  dispatched: "triangle",
  spawned: "circle",
  message: "diamond",
  report: "square",
};

export const arrowMarker = (kind: EdgeKind): ArrowMarker => markers[kind] ?? "diamond";

export type ArrowBox = {
  readonly x: number;
  /** Where it leaves and where it lands, from the top of the first lane. */
  readonly y1: number;
  readonly y2: number;
};

/**
 * An arrow on the chart: a vertical line at its moment, from the middle of the
 * sender's lane to the middle of the recipient's. The user has no lane: an
 * arrow from or to the user starts or ends at the axis, above the first lane.
 * `undefined` when it names an agent that has no lane.
 */
export function arrowBox(
  arrow: TimelineArrow,
  laneIndex: ReadonlyMap<string, number>,
  scale: TimeScale,
): ArrowBox | undefined {
  const row = (agentId: string | null): number | undefined => {
    if (agentId === null) return -6;
    const index = laneIndex.get(agentId);
    return index === undefined ? undefined : index * LANE_HEIGHT + LANE_HEIGHT / 2;
  };
  const y1 = row(arrow.from_agent_id);
  const y2 = row(arrow.to_agent_id);
  if (y1 === undefined || y2 === undefined || y1 === y2) return undefined;
  return { x: timeToX(scale, arrow.ts), y1, y2 };
}

const spanKindWords: Record<SpanKind, string> = {
  turn: "Turn",
  tool: "Tool call",
  waiting: "Waiting for you",
};

export const spanKindLabel = (kind: SpanKind): string => spanKindWords[kind] ?? kind;

/** How long a span lasted, or has lasted so far. */
export const spanDuration = (span: TimelineSpan, now: number): number =>
  Math.max(0, (span.end ?? now) - span.start);

/** What became of a span: a tool call's own status, otherwise whether it is over. */
export function spanStatus(span: TimelineSpan): string {
  if (span.status !== null) return itemLook(span.status).label;
  return span.end === null ? "In progress" : "Ended";
}

/** One span as a line of text: what, when, how long. */
export function spanSentence(span: TimelineSpan, now: number): string {
  const what =
    span.kind === "turn" ? span.title || "Turn" : `${spanKindLabel(span.kind)}: ${span.title}`;
  const length =
    span.end === null
      ? `${formatDuration(spanDuration(span, now))} so far`
      : formatDuration(spanDuration(span, now));
  return `${what}, ${clockTime(span.start)}, ${length}${span.status === null ? "" : `, ${spanStatus(span)}`}`;
}

export type LaneOutline = {
  readonly id: string;
  readonly label: string;
  readonly turns: readonly {
    id: string;
    text: string;
    inner: readonly { id: string; text: string }[];
  }[];
};

/** The timeline as text: per lane, its spans in order with their times and durations. */
export function describeTimeline(lanes: readonly LaneView[], now: number): LaneOutline[] {
  return lanes.map((view) => ({
    id: view.id,
    label: view.label,
    turns: nestSpans(view.lane.spans).map((group, index) => ({
      id: group.turn?.id ?? `loose-${index}`,
      text: group.turn ? spanSentence(group.turn, now) : "Outside any turn",
      inner: group.inner.map((span) => ({ id: span.id, text: spanSentence(span, now) })),
    })),
  }));
}

/** One arrow as a sentence. */
export function arrowSentence(
  arrow: TimelineArrow,
  nameOf: (agentId: string | null) => string,
): string {
  const label = arrow.label === null || arrow.label === "" ? "" : `: ${arrow.label}`;
  return `${clockTime(arrow.ts)}, ${arrowKindWords(arrow.kind)} from ${nameOf(arrow.from_agent_id)} to ${nameOf(arrow.to_agent_id)}${label}`;
}

/** Round moments for the axis, about one every 90 pixels, within `x0`..`x1`. */
export function axisTicks(scale: TimeScale, x0: number, x1: number): { x: number; ts: number }[] {
  const steps = [
    100, 250, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000,
    900_000, 1_800_000, 3_600_000, 7_200_000, 21_600_000, 43_200_000, 86_400_000,
  ];
  const wanted = 90 / scale.pxPerMs;
  const step = steps.find((one) => one >= wanted) ?? steps[steps.length - 1];
  const first = Math.ceil(xToTime(scale, Math.max(0, x0)) / step) * step;
  const ticks: { x: number; ts: number }[] = [];
  for (let ts = first; timeToX(scale, ts) <= x1 && ticks.length < 200; ts += step) {
    ticks.push({ x: timeToX(scale, ts), ts });
  }
  return ticks;
}
