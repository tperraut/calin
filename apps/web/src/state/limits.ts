import type { LimitWindow } from "../gen/LimitWindow";
import type { RunnerLimits } from "../gen/RunnerLimits";
import { compareText } from "../lib/format";

/**
 * Takes one runner's report into the list of the newest report per runner.
 * An older report than the one held changes nothing, so a replayed `limits`
 * event never undoes a newer one. Pure; the list stays sorted by runner.
 */
export function mergeLimits(
  list: readonly RunnerLimits[],
  report: RunnerLimits,
): readonly RunnerLimits[] {
  const held = list.find((one) => one.runner === report.runner);
  if (held && held.reported_at > report.reported_at) return list;
  return [...list.filter((one) => one.runner !== report.runner), report].sort((a, b) =>
    compareText(a.runner, b.runner),
  );
}

/** Takes every report of a `GET /usage/limits` answer; what the events already said newer stays. */
export function mergeAllLimits(
  list: readonly RunnerLimits[],
  reports: readonly RunnerLimits[],
): readonly RunnerLimits[] {
  return reports.reduce(mergeLimits, list);
}

/** The window closest to full across every runner, if any runner reported one. */
export function closestToFull(list: readonly RunnerLimits[]): LimitWindow | undefined {
  let best: LimitWindow | undefined;
  for (const report of list) {
    for (const window of report.windows) {
      if (!best || window.used > best.used) best = window;
    }
  }
  return best;
}

/** The window of `runner` that refuses requests now, the one resetting last when several do. */
export function refusingWindow(
  list: readonly RunnerLimits[],
  runner: string | undefined,
): LimitWindow | undefined {
  const report = list.find((one) => one.runner === runner);
  const refused = (report?.windows ?? []).filter((window) => window.refused);
  return refused.reduce<LimitWindow | undefined>(
    (found, window) => (!found || resetsAt(window) > resetsAt(found) ? window : found),
    undefined,
  );
}

/** A window with no reset time known resets last. */
const resetsAt = (window: LimitWindow): number => window.resets_at ?? Infinity;
