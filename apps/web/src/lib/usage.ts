import type { LimitWindow } from "../gen/LimitWindow";
import type { UsageRow } from "../gen/UsageRow";
import type { UsageTotals } from "../gen/UsageTotals";
import { relativeTime } from "./format";

/** The ranges the Usage page offers, in days (integrations spec, section 1). */
export const USAGE_RANGES = [7, 30, 90] as const;
export type UsageRange = (typeof USAGE_RANGES)[number];
export const DEFAULT_RANGE: UsageRange = 30;

/** The range a `?days=` value names; the default for anything else. */
export function rangeFromQuery(value: string | string[] | undefined): UsageRange {
  const days = Number(Array.isArray(value) ? value[0] : value);
  return USAGE_RANGES.find((one) => one === days) ?? DEFAULT_RANGE;
}

/** The last `days` days, today included: from the start of the first one, in local time, to `now`. */
export function rangeBounds(days: number, now: number): { from: number; to: number } {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  start.setDate(start.getDate() - (days - 1));
  return { from: start.getTime(), to: now };
}

/** `YYYY-MM-DD` of `ts` in local time, the form `by=day` keys its rows with. */
export function dayKey(ts: number): string {
  const date = new Date(ts);
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** Noon of a `YYYY-MM-DD` day in local time; noon stays inside the day across a clock change. */
export function dayNoon(key: string): number | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(key);
  if (!match) return undefined;
  return new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]), 12).getTime();
}

const ZERO: UsageTotals = { cost_usd: 0, tokens_in: 0, tokens_out: 0, sessions: 0 };

/**
 * The days of a `by=day` answer, oldest first, with any day missing between
 * the first and the last one added as an empty day: a day with nothing is a
 * slot of its own, never skipped. Rows that name no date are dropped.
 */
export function fillDays(rows: readonly UsageRow[]): UsageRow[] {
  const known = rows
    .filter((row) => dayNoon(row.key) !== undefined)
    .toSorted((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  const first = known.at(0);
  const last = known.at(-1);
  if (!first || !last) return [];
  const byKey = new Map(known.map((row) => [row.key, row]));
  const days: UsageRow[] = [];
  const cursor = new Date(dayNoon(first.key) ?? 0);
  // Bounded by the server's 366 days, and by the last key in any case.
  for (let guard = 0; guard < 400; guard += 1) {
    const key = dayKey(cursor.getTime());
    days.push(byKey.get(key) ?? { key, label: key, totals: ZERO });
    if (key >= last.key) break;
    cursor.setDate(cursor.getDate() + 1);
  }
  return days;
}

/** A value axis: its top and its ticks from 0 up, evenly spaced. */
export type Scale = { max: number; step: number; ticks: number[] };

/** Rounds away float noise such as 0.30000000000000004. */
const clean = (value: number): number => Number(value.toPrecision(12));

/**
 * A "nice" axis for values up to `max`: a step of 1, 2, 2.5 or 5 times a power
 * of ten, and a top that is a whole number of steps at or above `max`, which
 * gives three or four ticks. Nothing (0, negative, not finite) gets 0 to 1.
 */
export function niceScale(max: number): Scale {
  if (!Number.isFinite(max) || max <= 0) return { max: 1, step: 0.5, ticks: [0, 0.5, 1] };
  const raw = max / 3;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = clean(([1, 2, 2.5, 5, 10].find((m) => m * power >= raw * (1 - 1e-9)) ?? 10) * power);
  const intervals = Math.max(1, Math.ceil(clean(max / step)));
  const ticks = Array.from({ length: intervals + 1 }, (_, i) => clean(i * step));
  return { max: clean(intervals * step), step, ticks };
}

/** Height of a bar for `value` on `scale`, out of `height`; a value above 0 always shows. */
export function barHeight(value: number, scale: Scale, height: number, minimum = 2): number {
  if (!(value > 0)) return 0;
  return Math.min(height, Math.max(minimum, (value / scale.max) * height));
}

/** Decimals a step needs once float noise is gone: 0.25 → 2, 5 → 0, 0.0005 → 4. */
function decimalsOf(step: number): number {
  for (let decimals = 0; decimals < 10; decimals += 1) {
    if (Math.abs(clean(step * 10 ** decimals) - Math.round(step * 10 ** decimals)) < 1e-9) {
      return decimals;
    }
  }
  return 10;
}

/**
 * An axis label in dollars with the decimals the step needs, and whole cents
 * once there is any fraction: "$0", "$25", "$0.50", "$2.50", "$0.002".
 */
export function axisCost(value: number, step: number): string {
  const needed = decimalsOf(step);
  return `$${value.toFixed(needed === 0 ? 0 : Math.max(2, needed))}`;
}

/** "Tue 24 Nov" style: the date of a day key in the viewer's locale. */
export function dayLabel(key: string, style: "short" | "long" = "short"): string {
  const noon = dayNoon(key);
  if (noon === undefined) return key;
  return new Date(noon).toLocaleDateString(
    undefined,
    style === "long"
      ? { weekday: "long", day: "numeric", month: "long", year: "numeric" }
      : { day: "numeric", month: "short" },
  );
}

/** Which day labels show under `count` bars: today's always, then every `step`-th back from it. */
export function labelStep(count: number): number {
  if (count <= 10) return 1;
  if (count <= 31) return 5;
  return 14;
}

// --- Plan limits -----------------------------------------------------------

const numberWords: Record<string, number> = {
  one: 1,
  two: 2,
  three: 3,
  four: 4,
  five: 5,
  six: 6,
  seven: 7,
  eight: 8,
  ten: 10,
  twelve: 12,
  thirty: 30,
};

/**
 * The window's name in words: `five_hour` is "5-hour window", `seven_day` is
 * "7-day window", `seven_day_opus` is "7-day window (opus)". A name of another
 * shape is shown as the CLI gave it.
 */
export function windowName(name: string): string {
  const match = /^([a-z]+|\d+)_(hour|day|week|month)(?:_([a-z0-9_]+))?$/.exec(name);
  if (!match) return name;
  const count = /^\d+$/.test(match[1] ?? "") ? Number(match[1]) : numberWords[match[1] ?? ""];
  if (count === undefined) return name;
  const scope = match[3] === undefined ? "" : ` (${match[3].replaceAll("_", " ")})`;
  return `${count}-${match[2]} window${scope}`;
}

/** 0.623 → "62%", clamped to 0–100. */
export const percentUsed = (used: number): string =>
  `${Math.round(Math.min(1, Math.max(0, Number.isFinite(used) ? used : 0)) * 100)}%`;

const clock = (ts: number): string =>
  new Date(ts).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });

/**
 * When `ts` is, said from `now`: "18:00" today, "Thursday 09:00" within the
 * week, "3 December 09:00" beyond it. Local time.
 */
export function momentText(ts: number, now: number): string {
  if (dayKey(ts) === dayKey(now)) return clock(ts);
  const days = (ts - now) / 86_400_000;
  if (days > 0 && days < 6.5) {
    return `${new Date(ts).toLocaleDateString(undefined, { weekday: "long" })} ${clock(ts)}`;
  }
  return `${new Date(ts).toLocaleDateString(undefined, { day: "numeric", month: "long" })} ${clock(ts)}`;
}

/** "resets at 18:00", "resets Thursday 09:00", or nothing without a reset time. */
export function resetText(window: LimitWindow, now: number): string | undefined {
  if (window.resets_at === null) return undefined;
  const when = momentText(window.resets_at, now);
  return dayKey(window.resets_at) === dayKey(now) ? `resets at ${when}` : `resets ${when}`;
}

/** "Requests are being refused until 18:00." */
export function refusalText(window: LimitWindow, now: number): string {
  return window.resets_at === null
    ? "Requests are being refused."
    : `Requests are being refused until ${momentText(window.resets_at, now)}.`;
}

/** "reported 3 min ago", "reported just now". */
export const reportedText = (ts: number, now: number): string =>
  `reported ${relativeTime(ts, now)}`;

export type MeterState = { label: string; tone: "success" | "attention" | "danger" };

/** The state a meter is drawn in, always with its word. */
export function meterState(window: LimitWindow): MeterState {
  if (window.refused) return { label: "Refusing requests", tone: "danger" };
  if (window.used >= 0.9) return { label: "Nearly full", tone: "danger" };
  if (window.used >= 0.75) return { label: "Filling up", tone: "attention" };
  return { label: "Room left", tone: "success" };
}

/** "5-hour window 62% used", as the home summary says it. */
export const windowSummary = (window: LimitWindow): string =>
  `${windowName(window.name)} ${percentUsed(window.used)} used`;
