import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import { choosing, markedAt, withModifier, zoomKeyFactor } from "./canvas";
import { AXIS_HEIGHT, LANE_HEIGHT, spanKey, type LaneView, type TimeScale } from "./timelineModel";

// Where the timeline's view goes: its zoom, its scroll, and the keys that move through it.

/** What the reader picked in the timeline: a span of one agent's lane, or an arrow. */
export type TimelineSelection =
  | { kind: "span"; agentId: string; id: string }
  | { kind: "arrow"; id: string };

export const ZOOM_STEP = 1.5;
/** The widest the chart may get: browsers stop drawing far beyond this. */
const MAX_CONTENT_WIDTH = 1_500_000;

/** The width of the lane column: narrower on a narrow screen. */
export const laneColumnWidth = (width: number): number => (width > 0 && width < 520 ? 104 : 176);

/** The pixels the chart has beside the lane column; a phone's width until the screen is measured. */
export const chartRoom = (width: number): number =>
  Math.max(120, (width || 360) - laneColumnWidth(width));

/**
 * The zoom asked for, within what the chart can draw. `undefined` is the fit:
 * at or below it, the whole history fits the viewport, and keeps fitting as it grows.
 */
export function clampZoom(
  pxPerMs: number | undefined,
  fit: number,
  durationMs: number,
): number | undefined {
  if (pxPerMs === undefined || pxPerMs <= fit) return undefined;
  const ceiling = MAX_CONTENT_WIDTH / Math.max(1000, durationMs);
  return Math.min(pxPerMs, Math.max(fit, ceiling));
}

/** The scale at a zoom: the fitted one while there is none. */
export const zoomedScale = (zoom: number | undefined, fitted: TimeScale): TimeScale =>
  zoom === undefined ? fitted : { from: fitted.from, pxPerMs: zoom };

/** What a scroll box shows: its scroll, and its size. */
export type Viewport = { left: number; top: number; width: number; height: number };

/** The scroll that shows the lane `row`: unchanged while it is in view, else its nearest edge. */
function revealTop(row: number, view: Viewport): number {
  const top = row * LANE_HEIGHT;
  if (top < view.top) return top;
  const bottom = top + LANE_HEIGHT + AXIS_HEIGHT;
  return bottom > view.top + view.height ? bottom - view.height : view.top;
}

/** The scroll that shows the pixel `x` of the axis in lane `row`; out of view, `x` lands a third of the way in. */
export function revealScroll(
  x: number,
  row: number,
  view: Viewport,
): { left: number; top: number } {
  const outside = x < view.left + 16 || x > view.left + view.width - 48;
  return { left: outside ? Math.max(0, x - view.width / 3) : view.left, top: revealTop(row, view) };
}

/** The key of what the reader picked, when it is of `kind`: a span's key, or an arrow's id. */
export function selectedKey(
  selection: TimelineSelection | undefined,
  kind: TimelineSelection["kind"],
): string | undefined {
  if (selection?.kind !== kind) return undefined;
  return selection.kind === "span" ? spanKey(selection.agentId, selection.id) : selection.id;
}

/** Where a span is: its lane's row, and its place in the lane. */
type SpanAt = { row: number; index: number };

export function findSpan(lanes: readonly LaneView[], key: string): SpanAt | undefined {
  for (const [row, lane] of lanes.entries()) {
    const index = lane.lane.spans.findIndex((span) => spanKey(lane.id, span.id) === key);
    if (index !== -1) return { row, index };
  }
  return undefined;
}

/** The span nearest in time to `start` in the next lane from `row`, one way or the other, that has any. */
export function nearestAcross(
  lanes: readonly LaneView[],
  row: number,
  by: number,
  start: number,
): { agentId: string; span: TimelineSpan } | undefined {
  for (let next = row + by; next >= 0 && next < lanes.length; next += by) {
    const other = lanes[next];
    const nearest = other.lane.spans.toSorted(
      (a, b) => Math.abs(a.start - start) - Math.abs(b.start - start),
    )[0];
    if (nearest) return { agentId: other.id, span: nearest };
  }
  return undefined;
}

/** What a key asks of the timeline; `stay` keeps the key from the browser and does nothing. */
export type TimelineCommand =
  | { kind: "zoom"; factor: number }
  | { kind: "select"; selection: TimelineSelection }
  | { kind: "span"; agentId: string; span: TimelineSpan }
  | { kind: "arrow"; arrow: TimelineArrow }
  | { kind: "stay" };

const sideSteps: ReadonlyMap<string, number> = new Map([
  ["ArrowRight", 1],
  ["ArrowLeft", -1],
]);
const laneSteps: ReadonlyMap<string, number> = new Map([
  ["ArrowDown", 1],
  ["ArrowUp", -1],
]);

/** On an arrow: Left and Right step through `arrows`, Enter or Space picks it. Past either end, nothing. */
function arrowKey(
  key: string,
  id: string,
  arrows: readonly TimelineArrow[],
): TimelineCommand | undefined {
  if (choosing(key)) return { kind: "select", selection: { kind: "arrow", id } };
  const step = sideSteps.get(key);
  const next =
    step === undefined ? undefined : arrows[arrows.findIndex((arrow) => arrow.id === id) + step];
  return next && { kind: "arrow", arrow: next };
}

const focusOn = (target: { agentId: string; span: TimelineSpan } | undefined): TimelineCommand =>
  target ? { kind: "span", ...target } : { kind: "stay" };

/**
 * On a span: Left and Right step along its lane, Up and Down go to the nearest
 * span of the next lane that has any, Enter or Space picks it. At an end the
 * key does nothing, and the page still keeps it.
 */
function spanKeyCommand(
  key: string,
  lanes: readonly LaneView[],
  at: SpanAt,
): TimelineCommand | undefined {
  const lane = lanes[at.row];
  const current = lane.lane.spans[at.index];
  if (choosing(key)) {
    return { kind: "select", selection: { kind: "span", agentId: lane.id, id: current.id } };
  }
  const side = sideSteps.get(key);
  if (side !== undefined) {
    const next = lane.lane.spans[at.index + side];
    return focusOn(next && { agentId: lane.id, span: next });
  }
  const across = laneSteps.get(key);
  return across === undefined
    ? undefined
    : focusOn(nearestAcross(lanes, at.row, across, current.start));
}

/** The command of a key pressed on the chart; undefined leaves the key to the browser. */
export function timelineKey(
  event: KeyboardEvent,
  lanes: readonly LaneView[],
  arrows: readonly TimelineArrow[],
): TimelineCommand | undefined {
  if (withModifier(event)) return undefined;
  const factor = zoomKeyFactor(event.key, ZOOM_STEP);
  if (factor !== undefined) return { kind: "zoom", factor };
  const arrow = markedAt(event.target, "data-arrow");
  if (arrow !== undefined) return arrowKey(event.key, arrow, arrows);
  const span = markedAt(event.target, "data-span");
  const at = span === undefined ? undefined : findSpan(lanes, span);
  return at && spanKeyCommand(event.key, lanes, at);
}
