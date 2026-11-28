import { createMemo, createSignal, For, onSettled, Show } from "solid-js";
import type { UsageRow } from "../gen/UsageRow";
import { compactCount, formatCost } from "../lib/format";
import { axisCost, barHeight, dayLabel, labelStep, niceScale } from "../lib/usage";

/**
 * Narrowest a day's slot gets on a touch screen before the chart scrolls (see
 * `--chart-slot` in index.css): the bar's hit area is the whole slot, the plot's height.
 */
export const MIN_SLOT_PX = 24;
const PLOT_PX = 160;
const TOP_PX = 8;
const LABELS_PX = 22;
const AXIS_PX = 52;
const HEIGHT = TOP_PX + PLOT_PX + LABELS_PX;
/** Share of a slot the bar fills; the rest is the gap between bars. */
const BAR_SHARE = 0.68;
const RADIUS = 3;

const pct = (value: number): string => `${value}%`;

/** What a day says in words, for its bar, the detail line and the table. */
export function dayText(row: UsageRow, today: boolean): string {
  const totals = row.totals;
  return (
    `${dayLabel(row.key, "long")}${today ? " (today)" : ""}: ${formatCost(totals.cost_usd)}, ` +
    `${compactCount(totals.tokens_in)} tokens in, ${compactCount(totals.tokens_out)} tokens out`
  );
}

/** The day the arrow keys, Home and End lead to from `index`; `undefined` for any other key. */
function nextDay(key: string, index: number, last: number): number | undefined {
  switch (key) {
    case "ArrowLeft":
      return Math.max(0, index - 1);
    case "ArrowRight":
      return Math.min(last, index + 1);
    case "Home":
      return 0;
    case "End":
      return last;
    default:
      return undefined;
  }
}

/** Where a day's label sits: the first and last ones flush with the plot's edges, so they are not cut. */
function labelPlace(
  index: number,
  count: number,
  center: number,
): { x: string; anchor: "start" | "middle" | "end" } {
  if (index === 0) return { x: "0", anchor: "start" };
  if (index === count - 1) return { x: "100%", anchor: "end" };
  return { x: pct(center), anchor: "middle" };
}

/** The words for the day at `index`, when there is one. */
function dayDetail(
  days: readonly UsageRow[],
  index: number | undefined,
  today: number,
): string | undefined {
  const row = index === undefined ? undefined : days[index];
  return row ? dayText(row, index === today) : undefined;
}

/** The value axis, beside the plot so it stays put while the days scroll. */
function ValueAxis(props: {
  ticks: readonly number[];
  step: number;
  y: (value: number) => number;
}) {
  return (
    <svg
      aria-hidden="true"
      width={AXIS_PX}
      height={HEIGHT}
      class="shrink-0 overflow-visible text-fg"
      data-part="value-axis"
    >
      <For each={props.ticks}>
        {(tick) => (
          <text
            x={AXIS_PX - 6}
            y={props.y(tick)}
            text-anchor="end"
            dominant-baseline="middle"
            font-size="11"
            fill="currentColor"
          >
            {axisCost(tick, props.step)}
          </text>
        )}
      </For>
    </svg>
  );
}

function GridLines(props: { ticks: readonly number[]; y: (value: number) => number }) {
  return (
    <g aria-hidden="true">
      <For each={props.ticks}>
        {(tick) => (
          <line
            x1="0"
            x2="100%"
            y1={props.y(tick)}
            y2={props.y(tick)}
            stroke="var(--line)"
            stroke-width="1"
            stroke-dasharray={tick === 0 ? undefined : "2 3"}
          />
        )}
      </For>
    </g>
  );
}

/** One day's bar, or a stub on the baseline when it cost nothing. `x` and `width` are percents. */
function Bar(props: { x: number; width: number; height: number }) {
  return (
    <Show
      when={props.height > 0}
      fallback={
        // An empty day keeps its slot: a stub on the baseline says "nothing", not "missing".
        <rect
          aria-hidden="true"
          data-part="empty"
          x={pct(props.x)}
          width={pct(props.width)}
          y={TOP_PX + PLOT_PX - 1}
          height="1"
          fill="var(--muted)"
        />
      }
    >
      <rect
        aria-hidden="true"
        data-part="bar"
        x={pct(props.x)}
        width={pct(props.width)}
        y={TOP_PX + PLOT_PX - props.height}
        height={props.height}
        rx={Math.min(RADIUS, props.height / 2)}
        fill="var(--accent)"
      />
      {/* Squares the bottom corners: only the data end is rounded. */}
      <Show when={props.height > RADIUS}>
        <rect
          aria-hidden="true"
          x={pct(props.x)}
          width={pct(props.width)}
          y={TOP_PX + PLOT_PX - RADIUS}
          height={RADIUS}
          fill="var(--accent)"
        />
      </Show>
    </Show>
  );
}

type SlotProps = {
  day: UsageRow;
  index: number;
  count: number;
  /** Width of one day's slot, in percent of the plot. */
  slot: number;
  height: number;
  today: boolean;
  shown: boolean;
  focused: boolean;
  tabStop: boolean;
  labelled: boolean;
  ref: (element: SVGRectElement) => void;
  onPointerEnter: () => void;
  onFocus: () => void;
  onBlur: () => void;
  onKeyDown: (event: KeyboardEvent) => void;
};

/** One day's slot: its shade and focus ring, its bar, the hit area the reader acts on, its label. */
function DaySlot(props: SlotProps) {
  const left = (): number => props.index * props.slot;
  const place = () => labelPlace(props.index, props.count, left() + props.slot / 2);
  return (
    <g data-part="day" data-day={props.day.key} data-today={props.today ? "" : undefined}>
      <Show when={props.shown}>
        <rect
          aria-hidden="true"
          x={pct(left())}
          width={pct(props.slot)}
          y={TOP_PX}
          height={PLOT_PX}
          fill="var(--sunken)"
        />
      </Show>
      <Bar
        x={left() + (props.slot * (1 - BAR_SHARE)) / 2}
        width={props.slot * BAR_SHARE}
        height={props.height}
      />
      <Show when={props.focused}>
        <rect
          aria-hidden="true"
          x={pct(left())}
          width={pct(props.slot)}
          y={TOP_PX}
          height={PLOT_PX}
          fill="none"
          stroke="var(--fg)"
          stroke-width="2"
          rx="3"
        />
      </Show>
      {/* The hit area: the whole slot, taller and wider than the bar. */}
      <rect
        ref={props.ref}
        role="img"
        aria-label={dayText(props.day, props.today)}
        tabindex={props.tabStop ? 0 : -1}
        data-part="hit"
        x={pct(left())}
        width={pct(props.slot)}
        y={TOP_PX}
        height={PLOT_PX}
        fill="transparent"
        class="outline-none"
        onPointerEnter={() => props.onPointerEnter()}
        onFocus={() => props.onFocus()}
        onBlur={() => props.onBlur()}
        onKeyDown={(event) => props.onKeyDown(event)}
      >
        <title>{dayText(props.day, props.today)}</title>
      </rect>
      <Show when={props.labelled || props.today}>
        <text
          aria-hidden="true"
          data-part="day-label"
          x={place().x}
          y={TOP_PX + PLOT_PX + 15}
          text-anchor={place().anchor}
          font-size="11"
          font-weight={props.today ? "600" : undefined}
          fill="currentColor"
        >
          {props.today ? "Today" : dayLabel(props.day.key)}
        </text>
      </Show>
    </g>
  );
}

/** The same numbers as the bars, for screen readers. */
function DayTable(props: { days: readonly UsageRow[]; today: string | undefined }) {
  return (
    // Clipped by a wrapper: a table keeps the size of its cells whatever width it is given.
    <div class="sr-only">
      <table data-part="day-table">
        <caption>Cost by day</caption>
        <thead>
          <tr>
            <th scope="col">Day</th>
            <th scope="col">Cost</th>
            <th scope="col">Tokens in</th>
            <th scope="col">Tokens out</th>
          </tr>
        </thead>
        <tbody>
          <For each={props.days} keyed={(day) => day.key}>
            {(day) => (
              <tr>
                <th scope="row">
                  {dayLabel(day().key, "long")}
                  {day().key === props.today ? " (today)" : ""}
                </th>
                <td>{formatCost(day().totals.cost_usd)}</td>
                <td>{day().totals.tokens_in}</td>
                <td>{day().totals.tokens_out}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

/**
 * Cost per day as one bar series, to scale on a "nice" axis. Every day has its
 * slot, empty ones included. When the days do not fit at `MIN_SLOT_PX` each,
 * the plot scrolls inside its box while the value axis stays put. A bar is
 * focusable (arrow keys move between days) and names its day, cost and
 * tokens; the same numbers are in a table for screen readers.
 */
export function DayChart(props: { days: readonly UsageRow[]; today: string | undefined }) {
  const scale = createMemo(() =>
    niceScale(Math.max(0, ...props.days.map((d) => d.totals.cost_usd))),
  );
  const count = (): number => props.days.length;
  const slot = (): number => 100 / Math.max(1, count());
  const todayIndex = (): number => props.days.findIndex((day) => day.key === props.today);
  // Today, or the last day when the range has none: where the labels count from, and the way in.
  const anchor = (): number => (todayIndex() >= 0 ? todayIndex() : count() - 1);
  const labelled = (index: number): boolean => (anchor() - index) % labelStep(count()) === 0;
  const yOf = (value: number): number => TOP_PX + PLOT_PX - (value / scale().max) * PLOT_PX;

  // The bar the keyboard is on (one tab stop for the whole chart), and the one pointed at.
  const [cursor, setCursor] = createSignal<number>();
  const [hovered, setHovered] = createSignal<number>();
  const [focused, setFocused] = createSignal<number>();
  const shown = (): number | undefined => hovered() ?? focused();
  const tabStop = (): number => cursor() ?? anchor();
  const bars: SVGRectElement[] = [];
  let scroller: HTMLDivElement | undefined;

  onSettled(() => {
    // The newest days are the ones looked for: start scrolled to them.
    if (scroller) scroller.scrollLeft = scroller.scrollWidth;
  });

  function move(event: KeyboardEvent, index: number): void {
    const next = nextDay(event.key, index, count() - 1);
    if (next === undefined) return;
    event.preventDefault();
    setCursor(next);
    bars[next]?.focus();
  }

  return (
    <div class="flex flex-col gap-1">
      <p class="min-h-10 text-sm" aria-hidden="true" data-part="day-detail">
        {dayDetail(props.days, shown(), todayIndex()) ?? (
          <span class="text-muted pointer-coarse:hidden">
            Point at a bar, or focus one and use the arrow keys.
          </span>
        )}
      </p>
      <div class="flex">
        <ValueAxis ticks={scale().ticks} step={scale().step} y={yOf} />
        <div
          ref={(element) => {
            scroller = element;
          }}
          class="min-w-0 flex-1 overflow-x-auto overscroll-x-contain"
          data-part="chart-scroll"
        >
          <svg
            role="group"
            aria-label="Cost by day, one bar per day"
            width="100%"
            height={HEIGHT}
            style={{ "min-width": `calc(${count()} * var(--chart-slot, ${MIN_SLOT_PX}px))` }}
            class="block text-fg"
            onPointerLeave={() => setHovered(undefined)}
          >
            <GridLines ticks={scale().ticks} y={yOf} />
            <For each={props.days} keyed={(day) => day.key}>
              {(day, index) => (
                <DaySlot
                  day={day()}
                  index={index()}
                  count={count()}
                  slot={slot()}
                  height={barHeight(day().totals.cost_usd, scale(), PLOT_PX)}
                  today={index() === todayIndex()}
                  shown={shown() === index()}
                  focused={focused() === index()}
                  tabStop={index() === tabStop()}
                  labelled={labelled(index())}
                  ref={(element) => {
                    bars[index()] = element;
                  }}
                  onPointerEnter={() => setHovered(index())}
                  onFocus={() => {
                    setFocused(index());
                    setCursor(index());
                  }}
                  onBlur={() => setFocused(undefined)}
                  onKeyDown={(event) => move(event, index())}
                />
              )}
            </For>
          </svg>
        </div>
      </div>
      <p class="text-xs text-muted" data-part="day-zone">
        Days are the server's local days.
      </p>
      <DayTable days={props.days} today={props.today} />
    </div>
  );
}
