const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "just now", "5 min ago", "3 h ago", "2 d ago", then a date. Times are Unix milliseconds. */
export function relativeTime(ts: number, now: number): string {
  const elapsed = Math.max(0, now - ts);
  if (elapsed < MINUTE) return "just now";
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)} min ago`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)} h ago`;
  if (elapsed < 7 * DAY) return `${Math.floor(elapsed / DAY)} d ago`;
  return new Date(ts).toLocaleDateString(undefined, { dateStyle: "medium" });
}

/** "in less than a minute", "in 12 min", "in 3 h", "in 2 d", then a date. For times ahead of `now`. */
export function relativeFuture(ts: number, now: number): string {
  const ahead = Math.max(0, ts - now);
  if (ahead < MINUTE) return "in less than a minute";
  if (ahead < HOUR) return `in ${Math.floor(ahead / MINUTE)} min`;
  if (ahead < DAY) return `in ${Math.floor(ahead / HOUR)} h`;
  if (ahead < 7 * DAY) return `in ${Math.floor(ahead / DAY)} d`;
  return `on ${new Date(ts).toLocaleDateString(undefined, { dateStyle: "medium" })}`;
}

export function absoluteTime(ts: number): string {
  return new Date(ts).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function trimZero(value: string): string {
  return value.endsWith(".0") ? value.slice(0, -2) : value;
}

/** 950 → "950", 1234 → "1.2k", 2500000 → "2.5M". */
export function compactCount(count: number): string {
  if (count < 1000) return String(count);
  if (count < 1_000_000) return `${trimZero((count / 1000).toFixed(1))}k`;
  return `${trimZero((count / 1_000_000).toFixed(1))}M`;
}

/** "1 repo", "3 repos": the count with the word that goes with it. */
export const counted = (count: number, one: string, many: string): string =>
  `${count} ${count === 1 ? one : many}`;

export const filesLabel = (count: number): string => counted(count, "file", "files");

/** Code-point order of two strings, for a sort that must not depend on the locale. */
export const compareText = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

/** Text typed in an optional field, trimmed; `null` when nothing is left, which the server reads as "not set". */
export const blankAsNull = (typed: string): string | null => typed.trim() || null;

/** Text trimmed, or `undefined` when there is nothing but blanks. */
export function filled(text: string | undefined): string | undefined {
  const trimmed = text?.trim();
  return trimmed === "" ? undefined : trimmed;
}

/** "api", "api and web", "api, web and docs". */
export function listInWords(items: readonly string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** Costs under a dollar keep four decimals, so a fraction of a cent does not read as free. */
export function formatCost(usd: number): string {
  return `$${usd.toFixed(usd > 0 && usd < 1 ? 4 : 2)}`;
}

/** The time of day to the second, for moments within one session: "14:03:27". */
export function clockTime(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

/** 850 → "0.9 s", 12_400 → "12 s", 95_000 → "1 min 35 s", 3_720_000 → "1 h 2 min". */
export function formatDuration(ms: number): string {
  const total = Math.max(0, ms);
  // Rounded in tenths first: toFixed alone reads 0.85 (0.8499… as a float) as 0.8.
  if (total < 10_000) return `${trimZero((Math.round(total / 100) / 10).toFixed(1))} s`;
  const seconds = Math.round(total / 1000);
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60)
    return seconds % 60 === 0 ? `${minutes} min` : `${minutes} min ${seconds % 60} s`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 === 0 ? `${hours} h` : `${hours} h ${minutes % 60} min`;
}
