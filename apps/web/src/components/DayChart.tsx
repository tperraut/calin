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
  const todayIndex = (): number =>
    props.today === undefined ? -1 : props.days.findIndex((day) => day.key === props.today);
  const step = (): number => labelStep(count());
  const labelled = (index: number): boolean => {
    const anchor = todayIndex() >= 0 ? todayIndex() : count() - 1;
    return (anchor - index) % step() === 0;
  };
  const yOf = (value: number): number => TOP_PX + PLOT_PX - (value / scale().max) * PLOT_PX;

  // The bar the keyboard is on (one tab stop for the whole chart), and the one pointed at.
  const [cursor, setCursor] = createSignal<number>();
  const [hovered, setHovered] = createSignal<number>();
  const [focused, setFocused] = createSignal<number>();
  const shown = (): number | undefined => hovered() ?? focused();
  const tabStop = (): number => cursor() ?? (todayIndex() >= 0 ? todayIndex() : count() - 1);
  const bars: SVGRectElement[] = [];
  let scroller: HTMLDivElement | undefined;

  onSettled(() => {
    // The newest days are the ones looked for: start scrolled to them.
    if (scroller) scroller.scrollLeft = scroller.scrollWidth;
  });

  function move(event: KeyboardEvent, index: number): void {
    const last = count() - 1;
    const next =
      event.key === "ArrowLeft"
        ? Math.max(0, index - 1)
        : event.key === "ArrowRight"
          ? Math.min(last, index + 1)
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? last
              : undefined;
    if (next === undefined) return;
    event.preventDefault();
    setCursor(next);
    bars[next]?.focus();
  }

  const detail = (): string | undefined => {
    const index = shown();
    const row = index === undefined ? undefined : props.days[index];
    return row ? dayText(row, index === todayIndex()) : undefined;
  };

  return (
    <div class="flex flex-col gap-1">
      <p class="min-h-10 text-sm" aria-hidden="true" data-part="day-detail">
        {detail() ?? (
          <span class="text-muted pointer-coarse:hidden">
            Point at a bar, or focus one and use the arrow keys.
          </span>
        )}
      </p>
      <div class="flex">
        <svg
          aria-hidden="true"
          width={AXIS_PX}
          height={HEIGHT}
          class="shrink-0 overflow-visible text-fg"
          data-part="value-axis"
        >
          <For each={scale().ticks}>
            {(tick) => (
              <text
                x={AXIS_PX - 6}
                y={yOf(tick)}
                text-anchor="end"
                dominant-baseline="middle"
                font-size="11"
                fill="currentColor"
              >
                {axisCost(tick, scale().step)}
              </text>
            )}
          </For>
        </svg>
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
            <g aria-hidden="true">
              <For each={scale().ticks}>
                {(tick) => (
                  <line
                    x1="0"
                    x2="100%"
                    y1={yOf(tick)}
                    y2={yOf(tick)}
                    stroke="var(--line)"
                    stroke-width="1"
                    stroke-dasharray={tick === 0 ? undefined : "2 3"}
                  />
                )}
              </For>
            </g>
            <For each={props.days} keyed={(day) => day.key}>
              {(day, index) => {
                const height = (): number => barHeight(day().totals.cost_usd, scale(), PLOT_PX);
                const isToday = (): boolean => index() === todayIndex();
                const left = (): number => index() * slot();
                const barLeft = (): number => left() + (slot() * (1 - BAR_SHARE)) / 2;
                return (
                  <g data-part="day" data-day={day().key} data-today={isToday() ? "" : undefined}>
                    <Show when={shown() === index()}>
                      <rect
                        aria-hidden="true"
                        x={pct(left())}
                        width={pct(slot())}
                        y={TOP_PX}
                        height={PLOT_PX}
                        fill="var(--sunken)"
                      />
                    </Show>
                    <Show
                      when={height() > 0}
                      fallback={
                        // An empty day keeps its slot: a stub on the baseline says "nothing", not "missing".
                        <rect
                          aria-hidden="true"
                          data-part="empty"
                          x={pct(barLeft())}
                          width={pct(slot() * BAR_SHARE)}
                          y={TOP_PX + PLOT_PX - 1}
                          height="1"
                          fill="var(--muted)"
                        />
                      }
                    >
                      <rect
                        aria-hidden="true"
                        data-part="bar"
                        x={pct(barLeft())}
                        width={pct(slot() * BAR_SHARE)}
                        y={TOP_PX + PLOT_PX - height()}
                        height={height()}
                        rx={Math.min(RADIUS, height() / 2)}
                        fill="var(--accent)"
                      />
                      {/* Squares the bottom corners: only the data end is rounded. */}
                      <Show when={height() > RADIUS}>
                        <rect
                          aria-hidden="true"
                          x={pct(barLeft())}
                          width={pct(slot() * BAR_SHARE)}
                          y={TOP_PX + PLOT_PX - RADIUS}
                          height={RADIUS}
                          fill="var(--accent)"
                        />
                      </Show>
                    </Show>
                    <Show when={focused() === index()}>
                      <rect
                        aria-hidden="true"
                        x={pct(left())}
                        width={pct(slot())}
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
                      ref={(element) => {
                        bars[index()] = element;
                      }}
                      role="img"
                      aria-label={dayText(day(), isToday())}
                      tabindex={index() === tabStop() ? 0 : -1}
                      data-part="hit"
                      x={pct(left())}
                      width={pct(slot())}
                      y={TOP_PX}
                      height={PLOT_PX}
                      fill="transparent"
                      class="outline-none"
                      onPointerEnter={() => setHovered(index())}
                      onFocus={() => {
                        setFocused(index());
                        setCursor(index());
                      }}
                      onBlur={() => setFocused(undefined)}
                      onKeyDown={(event) => move(event, index())}
                    >
                      <title>{dayText(day(), isToday())}</title>
                    </rect>
                    <Show when={labelled(index()) || isToday()}>
                      <text
                        aria-hidden="true"
                        data-part="day-label"
                        x={
                          index() === 0
                            ? "0"
                            : index() === count() - 1
                              ? "100%"
                              : pct(left() + slot() / 2)
                        }
                        y={TOP_PX + PLOT_PX + 15}
                        text-anchor={
                          index() === 0 ? "start" : index() === count() - 1 ? "end" : "middle"
                        }
                        font-size="11"
                        font-weight={isToday() ? "600" : undefined}
                        fill="currentColor"
                      >
                        {isToday() ? "Today" : dayLabel(day().key)}
                      </text>
                    </Show>
                  </g>
                );
              }}
            </For>
          </svg>
        </div>
      </div>
      <p class="text-xs text-muted" data-part="day-zone">
        Days are the server's local days.
      </p>
      {/* Clipped by a wrapper: a table keeps the size of its cells whatever width it is given. */}
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
                    {props.today !== undefined && day().key === props.today ? " (today)" : ""}
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
    </div>
  );
}
