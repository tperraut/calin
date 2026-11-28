import { createMemo, createSignal, flush, For, onSettled, Show } from "solid-js";
import type { Timeline } from "../../gen/Timeline";
import type { TimelineArrow } from "../../gen/TimelineArrow";
import type { TimelineSpan } from "../../gen/TimelineSpan";
import { clockTime } from "../../lib/format";
import { createWidth } from "../../lib/media";
import { coalesce } from "../../lib/schedule";
import { at, flag, spread } from "./canvas";
import {
  arrowBox,
  arrowMarker,
  arrowSentence,
  AXIS_HEIGHT,
  axisTicks,
  cullRange,
  describeTimeline,
  drawnArrows,
  drawnSpans,
  firstKey,
  fitScale,
  LANE_HEIGHT,
  laneName,
  laneViews,
  seriesLegend,
  spanKey,
  spanLabel,
  spanSentence,
  timelineEnd,
  timeToX,
  TOOL_BAR,
  toolSeries,
  TURN_BAR,
  xToTime,
  type ArrowBox,
  type ArrowMarker,
  type DrawnSpan,
  type LaneOutline,
  type LaneView,
  type TimeScale,
} from "./timelineModel";
import {
  chartRoom,
  clampZoom,
  laneColumnWidth,
  revealScroll,
  selectedKey,
  timelineKey,
  zoomedScale,
  ZOOM_STEP,
  type TimelineCommand,
  type TimelineSelection,
} from "./timelineNav";

export type { TimelineSelection };

const END_PADDING = 24;

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

/** The element of the chart whose `data-<attribute>` is `value`. */
const markOf = (
  root: HTMLElement | undefined,
  attribute: "span" | "arrow",
  value: string,
): SVGGElement | undefined =>
  [...(root?.querySelectorAll<SVGGElement>(`[data-${attribute}]`) ?? [])].find(
    (one) => one.dataset[attribute] === value,
  );

/** The scroll box of the chart: what part of the axis it shows, and the moves that change it. */
function createChartScroll() {
  let element: HTMLDivElement | undefined;
  const [scrollX, setScrollX] = createSignal(0);
  /** Changes the scroll, once the box is in the document, and lets the culling follow. */
  const move = (change: (box: HTMLDivElement) => void): void => {
    if (!element) return;
    change(element);
    setScrollX(element.scrollLeft);
  };
  return {
    ref: (one: HTMLDivElement): void => {
      element = one;
    },
    element: (): HTMLDivElement | undefined => element,
    /** The pixel of the axis at the left of the viewport, as last scrolled to. */
    scrollX,
    onScroll: coalesce(() => move(() => {})),
    left: (): number => element?.scrollLeft ?? 0,
    scrollTo: (left: number): void =>
      move((box) => {
        box.scrollLeft = left;
      }),
    /** Scrolls so that the pixel `x` of the axis in lane `row` is in view, then lets the document follow. */
    reveal(x: number, row: number, room: number): void {
      move((box) => {
        const view = { left: box.scrollLeft, top: box.scrollTop };
        const next = revealScroll(x, row, { ...view, width: room, height: box.clientHeight });
        box.scrollLeft = next.left;
        box.scrollTop = next.top;
      });
      flush();
    },
    /** Moves the focus to the span or arrow drawn for `value`, without the browser's own scroll. */
    focus(attribute: "span" | "arrow", value: string): void {
      markOf(element, attribute, value)?.focus({ preventScroll: true });
    },
  };
}

type ChartScroll = ReturnType<typeof createChartScroll>;

/**
 * The zoom: `undefined` while the whole history fits the viewport, and keeps
 * fitting as it grows. A change keeps the moment under `anchor` (pixels into
 * the viewport) where it is.
 */
function createZoom(fitted: () => TimeScale, durationMs: () => number, scroll: ChartScroll) {
  const [zoom, setZoom] = createSignal<number>();
  const scale = createMemo(() => zoomedScale(zoom(), fitted()));
  function zoomTo(pxPerMs: number | undefined, anchor: number): void {
    const moment = xToTime(scale(), scroll.left() + anchor);
    setZoom(clampZoom(pxPerMs, fitted().pxPerMs, durationMs()));
    flush();
    scroll.scrollTo(Math.max(0, timeToX(scale(), moment) - anchor));
  }
  return {
    fitted: (): boolean => zoom() === undefined,
    scale,
    zoomTo,
    zoomAround: (factor: number, anchor: number): void => zoomTo(scale().pxPerMs * factor, anchor),
  };
}

/** Two fingers' spread, while there are exactly two. */
const fingers = (touches: TouchList): number | undefined =>
  touches.length === 2 ? spread(at(touches[0]), at(touches[1])) : undefined;

/**
 * Zooms around a point `x` pixels from the left of `element` with Ctrl or Meta
 * and the wheel, and with two fingers; returns what stops both.
 */
function observeZoomGestures(
  element: HTMLElement | undefined,
  zoomAround: (factor: number, x: number) => void,
): (() => void) | undefined {
  if (!element) return undefined;
  const zoomAt = (factor: number, clientX: number): void =>
    zoomAround(factor, clientX - element.getBoundingClientRect().left);
  // Not passive: with Ctrl (and a trackpad's pinch) the wheel zooms; alone it scrolls as usual.
  const onWheel = (event: WheelEvent): void => {
    if (!event.ctrlKey && !event.metaKey) return;
    event.preventDefault();
    zoomAt(Math.exp(-event.deltaY * 0.01), event.clientX);
  };
  let pinch: number | undefined;
  const onTouchStart = (event: TouchEvent): void => {
    pinch = fingers(event.touches);
  };
  const onTouchMove = (event: TouchEvent): void => {
    const now = fingers(event.touches);
    if (now === undefined || pinch === undefined) return;
    event.preventDefault();
    const middle = (event.touches[0].clientX + event.touches[1].clientX) / 2;
    if (pinch > 0 && now > 0) zoomAt(now / pinch, middle);
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
}

/** What the timeline's keys act on. */
type TimelineActions = {
  zoomBy: (factor: number) => void;
  select: (selection: TimelineSelection) => void;
  focusSpan: (agentId: string, span: TimelineSpan) => void;
  focusArrow: (arrow: TimelineArrow) => void;
};

function act(command: TimelineCommand, actions: TimelineActions): void {
  if (command.kind === "zoom") actions.zoomBy(command.factor);
  else if (command.kind === "select") actions.select(command.selection);
  else if (command.kind === "span") actions.focusSpan(command.agentId, command.span);
  else if (command.kind === "arrow") actions.focusArrow(command.arrow);
}

/** Acts on a key pressed on the chart and keeps it from the browser; any other key is left alone. */
function onTimelineKey(
  event: KeyboardEvent,
  lanes: readonly LaneView[],
  arrows: readonly TimelineArrow[],
  actions: TimelineActions,
): void {
  const command = timelineKey(event, lanes, arrows);
  if (!command) return;
  act(command, actions);
  event.preventDefault();
}

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
  const scroll = createChartScroll();
  const width = createWidth(scroll.element);
  const labelWidth = (): number => laneColumnWidth(width());
  const room = (): number => chartRoom(width());
  const lanes = createMemo(() => laneViews(props.timeline, props.openSessionId, props.titleOf));
  const laneIndex = createMemo(() => new Map(lanes().map((lane, index) => [lane.id, index])));
  const end = createMemo(() => timelineEnd(props.timeline, props.now));
  const fitted = createMemo(() => fitScale(props.timeline.from_ts, end(), room() - END_PADDING));
  const zoom = createZoom(fitted, () => end() - props.timeline.from_ts, scroll);
  const scale = zoom.scale;
  const contentWidth = createMemo(() =>
    Math.max(room(), Math.ceil(timeToX(scale(), end())) + END_PADDING),
  );
  const range = createMemo(() => cullRange(scroll.scrollX(), room()));
  const drawn = createMemo(() => drawnSpans(lanes(), scale(), props.now, range()));
  const arrows = createMemo(() =>
    drawnArrows(props.timeline.arrows, laneIndex(), scale(), range()),
  );
  const ticks = createMemo(() =>
    axisTicks(scale(), range().x0, Math.min(range().x1, contentWidth())),
  );
  const outline = createMemo(() => describeTimeline(lanes(), props.now));
  const nameOf = (agentId: string | null): string => laneName(lanes(), agentId);

  const [focusKey, setFocusKey] = createSignal<string>();
  const tabStop = (): string | undefined => focusKey() ?? firstKey(lanes());
  const zoomBy = (factor: number): void => zoom.zoomAround(factor, room() / 2);

  function focusSpan(agentId: string, span: TimelineSpan): void {
    const key = spanKey(agentId, span.id);
    setFocusKey(key);
    scroll.reveal(timeToX(scale(), span.start), laneIndex().get(agentId) ?? 0, room());
    scroll.focus("span", key);
  }

  function onKeyDown(event: KeyboardEvent): void {
    // Only the arrows the chart can place are stepped through, culled or not.
    const placed = props.timeline.arrows.filter((arrow) => arrowBox(arrow, laneIndex(), scale()));
    onTimelineKey(event, lanes(), placed, {
      zoomBy,
      select: (selection) => props.onSelect(selection),
      focusSpan,
      focusArrow: (arrow) => {
        scroll.reveal(timeToX(scale(), arrow.ts), 0, room());
        scroll.focus("arrow", arrow.id);
      },
    });
  }

  onSettled(() =>
    observeZoomGestures(scroll.element(), (factor, x) =>
      zoom.zoomAround(factor, Math.max(0, x - labelWidth())),
    ),
  );

  const chartHeight = (): number => lanes().length * LANE_HEIGHT;

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <div class="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-2 py-1">
        <ZoomButtons fitted={zoom.fitted()} zoomBy={zoomBy} fit={() => zoom.zoomTo(undefined, 0)} />
        <TimelineLegend />
      </div>
      <div
        ref={scroll.ref}
        class="relative min-h-0 flex-1 overflow-auto overscroll-contain bg-bg"
        style={{ "touch-action": "pan-x pan-y" }}
        onScroll={scroll.onScroll}
      >
        <div
          class="relative"
          style={{
            width: `${labelWidth() + contentWidth()}px`,
            height: `${AXIS_HEIGHT + chartHeight()}px`,
          }}
        >
          <LaneColumn lanes={lanes()} width={labelWidth()} height={AXIS_HEIGHT + chartHeight()} />
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
                  <SpanMark
                    one={one()}
                    name={nameOf(one().agentId)}
                    now={props.now}
                    selected={selectedKey(props.selection, "span") === one().key}
                    tabStop={tabStop() === one().key}
                    onFocus={() => setFocusKey(one().key)}
                    onSelect={() =>
                      props.onSelect({ kind: "span", agentId: one().agentId, id: one().span.id })
                    }
                  />
                )}
              </For>
              <For each={arrows()} keyed={(one) => one.arrow.id}>
                {(one, index) => (
                  <ArrowMark
                    arrow={one().arrow}
                    box={one().box}
                    sentence={arrowSentence(one().arrow, nameOf)}
                    selected={selectedKey(props.selection, "arrow") === one().arrow.id}
                    tabStop={index() === 0}
                    onSelect={() => props.onSelect({ kind: "arrow", id: one().arrow.id })}
                  />
                )}
              </For>
            </g>
          </svg>
        </div>
      </div>
      <TimelineText outline={outline()} arrows={props.timeline.arrows} nameOf={nameOf} />
    </div>
  );
}

function ZoomButtons(props: {
  fitted: boolean;
  zoomBy: (factor: number) => void;
  fit: () => void;
}) {
  return (
    <div class="flex gap-1">
      <button
        type="button"
        class="btn px-0 text-lg"
        aria-label="Zoom out"
        title="Zoom out (−)"
        disabled={props.fitted}
        onClick={() => props.zoomBy(1 / ZOOM_STEP)}
      >
        <span aria-hidden="true">−</span>
      </button>
      <button
        type="button"
        class="btn px-0 text-lg"
        aria-label="Zoom in"
        title="Zoom in (+)"
        onClick={() => props.zoomBy(ZOOM_STEP)}
      >
        <span aria-hidden="true">+</span>
      </button>
      <button
        type="button"
        class="btn px-2.5"
        title="Show the whole history"
        aria-pressed={props.fitted ? "true" : "false"}
        onClick={() => props.fit()}
      >
        Fit
      </button>
    </div>
  );
}

function TimelineLegend() {
  return (
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
  );
}

/** The lanes' names, kept in view at the left while the chart scrolls. */
function LaneColumn(props: { lanes: readonly LaneView[]; width: number; height: number }) {
  return (
    <div
      class="sticky left-0 z-10 border-r border-line bg-surface text-xs"
      style={{ width: `${props.width}px`, height: `${props.height}px` }}
    >
      <div
        class="flex items-center border-b border-line px-2 text-muted"
        style={{ height: `${AXIS_HEIGHT}px` }}
      >
        Agent
      </div>
      <For each={props.lanes} keyed={(lane) => lane.id}>
        {(lane) => (
          <div
            data-lane={lane().id}
            data-own={flag(lane().own)}
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
  );
}

/** A span, a button: a turn's or a wait's bar, or a tool call's with its label. */
function SpanMark(props: {
  one: DrawnSpan;
  /** Who the lane is, for the span's name. */
  name: string;
  now: number;
  selected: boolean;
  /** True for the one span the Tab key reaches. */
  tabStop: boolean;
  onFocus: () => void;
  onSelect: () => void;
}) {
  return (
    <g
      class="tl-span"
      data-span={props.one.key}
      data-kind={props.one.span.kind}
      data-status={props.one.span.status ?? undefined}
      data-open={flag(props.one.box.open)}
      data-selected={flag(props.selected)}
      role="button"
      tabindex={props.tabStop ? 0 : -1}
      aria-label={`${props.name}: ${spanSentence(props.one.span, props.now)}`}
      aria-pressed={props.selected ? "true" : "false"}
      transform={`translate(${props.one.box.x},${props.one.row * LANE_HEIGHT})`}
      onFocus={() => props.onFocus()}
      onClick={(event) => {
        event.stopPropagation();
        props.onSelect();
      }}
    >
      <Show
        when={props.one.span.kind === "tool"}
        fallback={<TurnBar waiting={props.one.span.kind === "waiting"} width={props.one.box.w} />}
      >
        <ToolBar span={props.one.span} width={props.one.box.w} />
      </Show>
    </g>
  );
}

const turnBar = { class: "tl-turn", y: TURN_BAR.y, h: TURN_BAR.h };
const waitBar = { class: "tl-waiting", y: TURN_BAR.y + 4, h: TURN_BAR.h - 8 };

/** A turn's bar, or a wait's, thinner and hatched. */
function TurnBar(props: { waiting: boolean; width: number }) {
  const bar = () => (props.waiting ? waitBar : turnBar);
  return <rect class={bar().class} y={bar().y} width={props.width} height={bar().h} rx="4" />;
}

/** A tool call's bar, colored by its kind, with its title when it fits. */
function ToolBar(props: { span: TimelineSpan; width: number }) {
  return (
    <>
      <rect
        class="tl-tool"
        data-series={toolSeries(props.span.tool_kind)}
        y={TOOL_BAR.y}
        // A hair of surface between two calls that touch.
        width={Math.max(2, props.width - 1)}
        height={TOOL_BAR.h}
        rx="3"
      />
      <Show when={spanLabel(props.span.title, props.width)}>
        {(label) => (
          <text class="tl-label" x="2" y={TOOL_BAR.y + TOOL_BAR.h + 11}>
            {label()}
          </text>
        )}
      </Show>
      {/* Taller and no narrower than a finger needs, whatever the bar's size. */}
      <rect
        class="tl-hit"
        x={Math.min(0, (props.width - 16) / 2)}
        width={Math.max(16, props.width)}
        height={LANE_HEIGHT}
      />
    </>
  );
}

/** An arrow, a button: a line from lane to lane, a dot where it leaves, a marker of its kind where it lands. */
function ArrowMark(props: {
  arrow: TimelineArrow;
  box: ArrowBox;
  sentence: string;
  selected: boolean;
  /** True for the one arrow the Tab key reaches. */
  tabStop: boolean;
  onSelect: () => void;
}) {
  return (
    <g
      class="tl-arrow"
      data-arrow={props.arrow.id}
      data-kind={props.arrow.kind}
      data-selected={flag(props.selected)}
      role="button"
      tabindex={props.tabStop ? 0 : -1}
      aria-label={props.sentence}
      aria-pressed={props.selected ? "true" : "false"}
      transform={`translate(${props.box.x},0)`}
      onClick={(event) => {
        event.stopPropagation();
        props.onSelect();
      }}
    >
      <line y1={props.box.y1} y2={props.box.y2} />
      <circle class="tl-marker" cy={props.box.y1} r="3" />
      <path
        class="tl-marker"
        transform={`translate(0,${props.box.y2})`}
        d={markerPath(arrowMarker(props.arrow.kind), props.box.y2 > props.box.y1)}
      />
      <rect class="tl-hit" x="-14" y={props.box.y2 - 14} width="28" height="28" />
    </g>
  );
}

/** The chart, as text: per lane, its spans in order with times and durations. */
function TimelineText(props: {
  outline: readonly LaneOutline[];
  arrows: readonly TimelineArrow[];
  nameOf: (agentId: string | null) => string;
}) {
  return (
    <div class="sr-only" data-part="timeline-text">
      <h3>Timeline as text</h3>
      <ul>
        <For each={props.outline} keyed={(lane) => lane.id}>
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
      <Show when={props.arrows.length > 0}>
        <h4>Messages and spawns</h4>
        <ul>
          <For each={props.arrows} keyed={(arrow) => arrow.id}>
            {(arrow) => <li>{arrowSentence(arrow(), props.nameOf)}</li>}
          </For>
        </ul>
      </Show>
    </div>
  );
}
