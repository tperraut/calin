import { createMemo, createSignal, flush, For, onSettled, Show } from "solid-js";
import type { Timeline } from "../../gen/Timeline";
import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import { clockTime } from "../../lib/format";
import { createWidth } from "../../lib/media";
import { coalesce } from "../../lib/schedule";
import {
  arrowBox,
  arrowMarker,
  arrowSentence,
  AXIS_HEIGHT,
  axisTicks,
  describeTimeline,
  fitScale,
  LANE_HEIGHT,
  laneViews,
  seriesLegend,
  spanBox,
  spanLabel,
  spanSentence,
  timelineEnd,
  timeToX,
  TOOL_BAR,
  toolSeries,
  TURN_BAR,
  visibleSpans,
  xToTime,
  type ArrowBox,
  type ArrowMarker,
  type SpanBox,
  type TimeScale,
} from "./timelineModel";

/** What the reader picked in the timeline: a span of one agent's lane, or an arrow. */
export type TimelineSelection =
  | { kind: "span"; agentId: string; id: string }
  | { kind: "arrow"; id: string };

const ZOOM_STEP = 1.5;
/** Spans this far outside the viewport are still drawn, so a short scroll shows no gap. */
const CULL_MARGIN = 240;
/** The widest the chart may get: browsers stop drawing far beyond this. */
const MAX_CONTENT_WIDTH = 1_500_000;
const END_PADDING = 24;

type DrawnSpan = {
  key: string;
  agentId: string;
  row: number;
  span: TimelineSpan;
  box: SpanBox;
};

const spanKey = (agentId: string, spanId: string): string => `${agentId}/${spanId}`;
// Turns under what lies inside them.
const drawOrder: Record<string, number> = { turn: 0, waiting: 1, tool: 2 };

const markerPath = (shape: ArrowMarker, down: boolean): string => {
  switch (shape) {
    case "triangle":
      return down ? "M-6,-7L6,-7L0,4z" : "M-6,7L6,7L0,-4z";
    case "circle":
      return "M0,-5.5a5.5,5.5 0 1,0 0.01,0z";
    case "square":
      return "M-5,-5H5V5H-5z";
    default:
      return "M0,-6.5L6.5,0L0,6.5L-6.5,0z";
  }
};

/**
 * Every agent's activity on one time axis: a lane per agent, turns as bars,
 * tool calls as shorter bars inside them, waits hatched, and arrows where
 * something crossed from one lane to another. Only what is near the viewport
 * is in the document.
 */
export function TimelineView(props: {
  timeline: Timeline;
  /** The session on screen: its lanes are marked. */
  openSessionId: string;
  /** A session's title, for the lane of its lead. */
  titleOf: (sessionId: string) => string | undefined;
  /** The present: open spans reach it. */
  now: number;
  selection: TimelineSelection | undefined;
  onSelect: (selection: TimelineSelection | undefined) => void;
}) {
  let scroller: HTMLDivElement | undefined;
  const width = createWidth(() => scroller);
  const labelWidth = (): number => (width() > 0 && width() < 520 ? 104 : 176);
  const room = (): number => Math.max(120, (width() || 360) - labelWidth());
  const lanes = createMemo(() => laneViews(props.timeline, props.openSessionId, props.titleOf));
  const laneIndex = createMemo(() => new Map(lanes().map((lane, index) => [lane.id, index])));
  const end = createMemo(() => timelineEnd(props.timeline, props.now));
  // `undefined`: the whole history fits the viewport, and keeps fitting as it grows.
  const [zoom, setZoom] = createSignal<number>();
  const fitted = createMemo(() => fitScale(props.timeline.from_ts, end(), room() - END_PADDING));
  const scale = createMemo<TimeScale>(() => {
    const chosen = zoom();
    return chosen === undefined ? fitted() : { from: props.timeline.from_ts, pxPerMs: chosen };
  });
  const contentWidth = createMemo(() =>
    Math.max(room(), Math.ceil(timeToX(scale(), end())) + END_PADDING),
  );
  const [scrollX, setScrollX] = createSignal(0);
  const range = createMemo(() => ({
    x0: scrollX() - CULL_MARGIN,
    x1: scrollX() + room() + CULL_MARGIN,
  }));

  const drawn = createMemo((): DrawnSpan[] => {
    const { x0, x1 } = range();
    const list: DrawnSpan[] = [];
    lanes().forEach((view, row) => {
      for (const span of visibleSpans(view.lane.spans, scale(), props.now, x0, x1)) {
        list.push({
          key: spanKey(view.id, span.id),
          agentId: view.id,
          row,
          span,
          box: spanBox(span, scale(), props.now),
        });
      }
    });
    return list.sort((a, b) => (drawOrder[a.span.kind] ?? 3) - (drawOrder[b.span.kind] ?? 3));
  });
  const arrows = createMemo(() => {
    const { x0, x1 } = range();
    const list: { arrow: TimelineArrow; box: ArrowBox }[] = [];
    for (const arrow of props.timeline.arrows) {
      const box = arrowBox(arrow, laneIndex(), scale());
      if (box && box.x >= x0 && box.x <= x1) list.push({ arrow, box });
    }
    return list;
  });
  const ticks = createMemo(() =>
    axisTicks(scale(), range().x0, Math.min(range().x1, contentWidth())),
  );
  const outline = createMemo(() => describeTimeline(lanes(), props.now));
  const nameOf = (agentId: string | null): string =>
    agentId === null ? "you" : (lanes().find((lane) => lane.id === agentId)?.label ?? "an agent");

  const [focusKey, setFocusKey] = createSignal<string>();
  const firstKey = (): string | undefined => {
    for (const lane of lanes()) {
      const span = lane.lane.spans.at(0);
      if (span) return spanKey(lane.id, span.id);
    }
    return undefined;
  };
  const tabStop = (): string | undefined => focusKey() ?? firstKey();
  const selectedSpan = (): string | undefined =>
    props.selection?.kind === "span"
      ? spanKey(props.selection.agentId, props.selection.id)
      : undefined;
  const selectedArrow = (): string | undefined =>
    props.selection?.kind === "arrow" ? props.selection.id : undefined;

  /** Changes the zoom keeping the moment under `anchor` (pixels into the viewport) where it is. */
  function zoomTo(pxPerMs: number | undefined, anchor: number): void {
    const before = scale();
    const moment = xToTime(before, (scroller?.scrollLeft ?? 0) + anchor);
    const floor = fitted().pxPerMs;
    const ceiling = MAX_CONTENT_WIDTH / Math.max(1000, end() - props.timeline.from_ts);
    const next =
      pxPerMs === undefined || pxPerMs <= floor
        ? undefined
        : Math.min(pxPerMs, Math.max(floor, ceiling));
    setZoom(next);
    flush();
    if (scroller) {
      scroller.scrollLeft = Math.max(0, timeToX(scale(), moment) - anchor);
      setScrollX(scroller.scrollLeft);
    }
  }
  const zoomBy = (factor: number, anchor = room() / 2): void =>
    zoomTo(scale().pxPerMs * factor, anchor);

  /** Scrolls so that the pixel `x` of the axis is in view, then lets the document follow. */
  function bringIntoView(x: number, row: number): void {
    if (!scroller) return;
    const left = scroller.scrollLeft;
    if (x < left + 16 || x > left + room() - 48) scroller.scrollLeft = Math.max(0, x - room() / 3);
    const top = row * LANE_HEIGHT;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + LANE_HEIGHT + AXIS_HEIGHT > scroller.scrollTop + scroller.clientHeight) {
      scroller.scrollTop = top + LANE_HEIGHT + AXIS_HEIGHT - scroller.clientHeight;
    }
    setScrollX(scroller.scrollLeft);
    flush();
  }

  function focusSpan(agentId: string, span: TimelineSpan): void {
    const row = laneIndex().get(agentId) ?? 0;
    const key = spanKey(agentId, span.id);
    setFocusKey(key);
    bringIntoView(timeToX(scale(), span.start), row);
    const element = [...(scroller?.querySelectorAll<SVGGElement>("[data-span]") ?? [])].find(
      (one) => one.dataset.span === key,
    );
    element?.focus({ preventScroll: true });
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "+" || event.key === "=") {
      zoomBy(ZOOM_STEP);
      event.preventDefault();
      return;
    }
    if (event.key === "-" || event.key === "_") {
      zoomBy(1 / ZOOM_STEP);
      event.preventDefault();
      return;
    }
    const target = event.target instanceof Element ? event.target : null;
    const arrowEl = target?.closest<SVGGElement>("[data-arrow]");
    if (arrowEl) {
      const all = props.timeline.arrows.filter((arrow) => arrowBox(arrow, laneIndex(), scale()));
      const at = all.findIndex((arrow) => arrow.id === arrowEl.dataset.arrow);
      const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
      if (event.key === "Enter" || event.key === " ") {
        props.onSelect({ kind: "arrow", id: all[at].id });
      } else if (step !== 0 && all[at + step]) {
        const next = all[at + step];
        bringIntoView(timeToX(scale(), next.ts), 0);
        [...(scroller?.querySelectorAll<SVGGElement>("[data-arrow]") ?? [])]
          .find((one) => one.dataset.arrow === next.id)
          ?.focus({ preventScroll: true });
      } else {
        return;
      }
      event.preventDefault();
      return;
    }
    const spanEl = target?.closest<SVGGElement>("[data-span]");
    if (!spanEl) return;
    const row = lanes().findIndex((lane) =>
      lane.lane.spans.some((span) => spanKey(lane.id, span.id) === spanEl.dataset.span),
    );
    if (row === -1) return;
    const lane = lanes()[row];
    const index = lane.lane.spans.findIndex(
      (span) => spanKey(lane.id, span.id) === spanEl.dataset.span,
    );
    const current = lane.lane.spans[index];
    const across = (by: number): void => {
      const next = lane.lane.spans[index + by];
      if (next) focusSpan(lane.id, next);
    };
    const along = (by: number): void => {
      // The next lane that has anything, and in it the span nearest in time.
      for (let at = row + by; at >= 0 && at < lanes().length; at += by) {
        const other = lanes()[at];
        const nearest = other.lane.spans.toSorted(
          (a, b) => Math.abs(a.start - current.start) - Math.abs(b.start - current.start),
        )[0];
        if (nearest) {
          focusSpan(other.id, nearest);
          return;
        }
      }
    };
    switch (event.key) {
      case "ArrowRight":
        across(1);
        break;
      case "ArrowLeft":
        across(-1);
        break;
      case "ArrowDown":
        along(1);
        break;
      case "ArrowUp":
        along(-1);
        break;
      case "Enter":
      case " ":
        props.onSelect({ kind: "span", agentId: lane.id, id: current.id });
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  onSettled(() => {
    const element = scroller;
    if (!element) return;
    // Not passive: with Ctrl (and a trackpad's pinch) the wheel zooms; alone it scrolls as usual.
    const onWheel = (event: WheelEvent): void => {
      if (!event.ctrlKey && !event.metaKey) return;
      event.preventDefault();
      const anchor = event.clientX - element.getBoundingClientRect().left - labelWidth();
      zoomBy(Math.exp(-event.deltaY * 0.01), Math.max(0, anchor));
    };
    let pinch: number | undefined;
    const spread = (event: TouchEvent): number =>
      Math.hypot(
        event.touches[0].clientX - event.touches[1].clientX,
        event.touches[0].clientY - event.touches[1].clientY,
      );
    const onTouchStart = (event: TouchEvent): void => {
      pinch = event.touches.length === 2 ? spread(event) : undefined;
    };
    const onTouchMove = (event: TouchEvent): void => {
      if (event.touches.length !== 2 || pinch === undefined) return;
      event.preventDefault();
      const now = spread(event);
      const middle =
        (event.touches[0].clientX + event.touches[1].clientX) / 2 -
        element.getBoundingClientRect().left -
        labelWidth();
      if (pinch > 0 && now > 0) zoomBy(now / pinch, Math.max(0, middle));
      pinch = now;
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    element.addEventListener("touchstart", onTouchStart, { passive: true });
    element.addEventListener("touchmove", onTouchMove, { passive: false });
    return () => {
      element.removeEventListener("wheel", onWheel);
      element.removeEventListener("touchstart", onTouchStart);
      element.removeEventListener("touchmove", onTouchMove);
    };
  });

  const onScroll = coalesce(() => {
    if (scroller) setScrollX(scroller.scrollLeft);
  });
  const chartHeight = (): number => lanes().length * LANE_HEIGHT;

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <div class="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-2 py-1">
        <div class="flex gap-1">
          <button
            type="button"
            class="btn px-0 text-lg"
            aria-label="Zoom out"
            title="Zoom out (−)"
            disabled={zoom() === undefined}
            onClick={() => zoomBy(1 / ZOOM_STEP)}
          >
            <span aria-hidden="true">−</span>
          </button>
          <button
            type="button"
            class="btn px-0 text-lg"
            aria-label="Zoom in"
            title="Zoom in (+)"
            onClick={() => zoomBy(ZOOM_STEP)}
          >
            <span aria-hidden="true">+</span>
          </button>
          <button
            type="button"
            class="btn px-2.5"
            title="Show the whole history"
            aria-pressed={zoom() === undefined ? "true" : "false"}
            onClick={() => zoomTo(undefined, 0)}
          >
            Fit
          </button>
        </div>
        <ul
          aria-label="Legend"
          class="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-0.5 text-xs text-muted"
        >
          <For each={seriesLegend}>
            {(entry) => (
              <li class="flex items-center gap-1">
                <svg width="10" height="10" aria-hidden="true">
                  <rect class="tl-tool" data-series={entry.series} width="10" height="10" rx="2" />
                </svg>
                {entry.label}
              </li>
            )}
          </For>
          <li class="flex items-center gap-1">
            <svg width="14" height="10" aria-hidden="true">
              <rect class="tl-waiting" width="14" height="10" rx="2" />
            </svg>
            Waiting for you
          </li>
        </ul>
      </div>
      <div
        ref={(element) => (scroller = element)}
        class="relative min-h-0 flex-1 overflow-auto overscroll-contain bg-bg"
        style={{ "touch-action": "pan-x pan-y" }}
        onScroll={onScroll}
      >
        <div
          class="relative"
          style={{
            width: `${labelWidth() + contentWidth()}px`,
            height: `${AXIS_HEIGHT + chartHeight()}px`,
          }}
        >
          <div
            class="sticky left-0 z-10 border-r border-line bg-surface text-xs"
            style={{ width: `${labelWidth()}px`, height: `${AXIS_HEIGHT + chartHeight()}px` }}
          >
            <div
              class="flex items-center border-b border-line px-2 text-muted"
              style={{ height: `${AXIS_HEIGHT}px` }}
            >
              Agent
            </div>
            <For each={lanes()} keyed={(lane) => lane.id}>
              {(lane) => (
                <div
                  data-lane={lane().id}
                  data-own={lane().own ? "" : undefined}
                  class="flex items-center border-b border-line pr-1.5 data-own:border-l-2 data-own:border-l-accent"
                  style={{
                    height: `${LANE_HEIGHT}px`,
                    "padding-left": `${6 + lane().depth * 10}px`,
                  }}
                  title={lane().label}
                >
                  <span class="line-clamp-2 min-w-0 wrap-anywhere">{lane().short}</span>
                </div>
              )}
            </For>
          </div>
          <svg
            class="absolute top-0 block"
            style={{ left: `${labelWidth()}px` }}
            width={contentWidth()}
            height={AXIS_HEIGHT + chartHeight()}
            role="group"
            aria-label="Timeline"
            onKeyDown={onKeyDown}
            onClick={(event) => {
              if (event.target === event.currentTarget) props.onSelect(undefined);
            }}
          >
            <defs>
              <pattern
                id="tl-hatch"
                width="6"
                height="6"
                patternUnits="userSpaceOnUse"
                patternTransform="rotate(45)"
              >
                <rect width="6" height="6" fill="var(--attention-soft)" />
                <rect width="2.5" height="6" fill="var(--tone-attention)" />
              </pattern>
            </defs>
            <For each={ticks()} keyed={(tick) => tick.ts}>
              {(tick) => (
                <g class="tl-tick" transform={`translate(${tick().x},0)`}>
                  <line y1={AXIS_HEIGHT - 6} y2={AXIS_HEIGHT + chartHeight()} />
                  <text x="3" y="12">
                    {clockTime(tick().ts)}
                  </text>
                </g>
              )}
            </For>
            <g transform={`translate(0,${AXIS_HEIGHT})`}>
              <For each={lanes()} keyed={(lane) => lane.id}>
                {(_lane, index) => (
                  <line
                    x1="0"
                    x2={contentWidth()}
                    y1={(index() + 1) * LANE_HEIGHT - 0.5}
                    y2={(index() + 1) * LANE_HEIGHT - 0.5}
                    stroke="var(--line)"
                    pointer-events="none"
                  />
                )}
              </For>
              <For each={drawn()} keyed={(one) => one.key}>
                {(one) => (
                  <g
                    class="tl-span"
                    data-span={one().key}
                    data-kind={one().span.kind}
                    data-status={one().span.status ?? undefined}
                    data-open={one().box.open ? "" : undefined}
                    data-selected={selectedSpan() === one().key ? "" : undefined}
                    role="button"
                    tabindex={tabStop() === one().key ? 0 : -1}
                    aria-label={`${nameOf(one().agentId)}: ${spanSentence(one().span, props.now)}`}
                    aria-pressed={selectedSpan() === one().key ? "true" : "false"}
                    transform={`translate(${one().box.x},${one().row * LANE_HEIGHT})`}
                    onFocus={() => setFocusKey(one().key)}
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onSelect({ kind: "span", agentId: one().agentId, id: one().span.id });
                    }}
                  >
                    <Show
                      when={one().span.kind === "tool"}
                      fallback={
                        <rect
                          class={one().span.kind === "waiting" ? "tl-waiting" : "tl-turn"}
                          y={one().span.kind === "waiting" ? TURN_BAR.y + 4 : TURN_BAR.y}
                          width={one().box.w}
                          height={one().span.kind === "waiting" ? TURN_BAR.h - 8 : TURN_BAR.h}
                          rx="4"
                        />
                      }
                    >
                      <rect
                        class="tl-tool"
                        data-series={toolSeries(one().span.tool_kind)}
                        y={TOOL_BAR.y}
                        // A hair of surface between two calls that touch.
                        width={Math.max(2, one().box.w - 1)}
                        height={TOOL_BAR.h}
                        rx="3"
                      />
                      <Show when={spanLabel(one().span.title, one().box.w)}>
                        {(label) => (
                          <text class="tl-label" x="2" y={TOOL_BAR.y + TOOL_BAR.h + 11}>
                            {label()}
                          </text>
                        )}
                      </Show>
                      {/* Taller and no narrower than a finger needs, whatever the bar's size. */}
                      <rect
                        class="tl-hit"
                        x={Math.min(0, (one().box.w - 16) / 2)}
                        width={Math.max(16, one().box.w)}
                        height={LANE_HEIGHT}
                      />
                    </Show>
                  </g>
                )}
              </For>
              <For each={arrows()} keyed={(one) => one.arrow.id}>
                {(one) => (
                  <g
                    class="tl-arrow"
                    data-arrow={one().arrow.id}
                    data-kind={one().arrow.kind}
                    data-selected={selectedArrow() === one().arrow.id ? "" : undefined}
                    role="button"
                    tabindex={arrows().at(0)?.arrow.id === one().arrow.id ? 0 : -1}
                    aria-label={arrowSentence(one().arrow, nameOf)}
                    aria-pressed={selectedArrow() === one().arrow.id ? "true" : "false"}
                    transform={`translate(${one().box.x},0)`}
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onSelect({ kind: "arrow", id: one().arrow.id });
                    }}
                  >
                    <line y1={one().box.y1} y2={one().box.y2} />
                    <circle class="tl-marker" cy={one().box.y1} r="3" />
                    <path
                      class="tl-marker"
                      transform={`translate(0,${one().box.y2})`}
                      d={markerPath(arrowMarker(one().arrow.kind), one().box.y2 > one().box.y1)}
                    />
                    <rect class="tl-hit" x="-14" y={one().box.y2 - 14} width="28" height="28" />
                  </g>
                )}
              </For>
            </g>
          </svg>
        </div>
      </div>
      {/* The chart, as text: per lane, its spans in order with times and durations. */}
      <div class="sr-only" data-part="timeline-text">
        <h3>Timeline as text</h3>
        <ul>
          <For each={outline()} keyed={(lane) => lane.id}>
            {(lane) => (
              <li>
                {lane().label}
                <ul>
                  <For each={lane().turns} keyed={(turn) => turn.id}>
                    {(turn) => (
                      <li>
                        {turn().text}
                        <Show when={turn().inner.length > 0}>
                          <ul>
                            <For each={turn().inner} keyed={(span) => span.id}>
                              {(span) => <li>{span().text}</li>}
                            </For>
                          </ul>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </li>
            )}
          </For>
        </ul>
        <Show when={props.timeline.arrows.length > 0}>
          <h4>Messages and spawns</h4>
          <ul>
            <For each={props.timeline.arrows} keyed={(arrow) => arrow.id}>
              {(arrow) => <li>{arrowSentence(arrow(), nameOf)}</li>}
            </For>
          </ul>
        </Show>
      </div>
    </div>
  );
}
