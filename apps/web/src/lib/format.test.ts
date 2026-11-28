import { describe, expect, it } from "vite-plus/test";
import {
  blankAsNull,
  clockTime,
  compactCount,
  compareText,
  filesLabel,
  formatCost,
  formatDuration,
  listInWords,
  relativeFuture,
  relativeTime,
} from "./format";

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NOW = Date.UTC(2026, 10, 28, 12, 0);
const date = (ts: number): string =>
  new Date(ts).toLocaleDateString(undefined, { dateStyle: "medium" });

describe("relativeTime", () => {
  it("says how long ago, in the largest whole unit, then the date from a week", () => {
    expect(relativeTime(NOW, NOW)).toBe("just now");
    expect(relativeTime(NOW - MINUTE + 1, NOW)).toBe("just now");
    expect(relativeTime(NOW - MINUTE, NOW)).toBe("1 min ago");
    expect(relativeTime(NOW - HOUR + 1, NOW)).toBe("59 min ago");
    expect(relativeTime(NOW - HOUR, NOW)).toBe("1 h ago");
    expect(relativeTime(NOW - DAY + 1, NOW)).toBe("23 h ago");
    expect(relativeTime(NOW - DAY, NOW)).toBe("1 d ago");
    expect(relativeTime(NOW - 7 * DAY + 1, NOW)).toBe("6 d ago");
    expect(relativeTime(NOW - 7 * DAY, NOW)).toBe(date(NOW - 7 * DAY));
  });

  it("reads a time ahead of now as now", () => {
    expect(relativeTime(NOW + HOUR, NOW)).toBe("just now");
  });
});

describe("relativeFuture", () => {
  it("says how long until, in the largest whole unit, then the date from a week", () => {
    expect(relativeFuture(NOW, NOW)).toBe("in less than a minute");
    expect(relativeFuture(NOW + MINUTE - 1, NOW)).toBe("in less than a minute");
    expect(relativeFuture(NOW + MINUTE, NOW)).toBe("in 1 min");
    expect(relativeFuture(NOW + HOUR - 1, NOW)).toBe("in 59 min");
    expect(relativeFuture(NOW + HOUR, NOW)).toBe("in 1 h");
    expect(relativeFuture(NOW + DAY - 1, NOW)).toBe("in 23 h");
    expect(relativeFuture(NOW + DAY, NOW)).toBe("in 1 d");
    expect(relativeFuture(NOW + 7 * DAY - 1, NOW)).toBe("in 6 d");
    expect(relativeFuture(NOW + 7 * DAY, NOW)).toBe(`on ${date(NOW + 7 * DAY)}`);
  });

  it("reads a time already past as now", () => {
    expect(relativeFuture(NOW - HOUR, NOW)).toBe("in less than a minute");
  });
});

describe("formatDuration", () => {
  it("keeps a tenth of a second under ten seconds, without a trailing .0", () => {
    expect(formatDuration(0)).toBe("0 s");
    expect(formatDuration(840)).toBe("0.8 s");
    // Halves round up, as the doc says: 0.85 is 0.8499… as a float, so toFixed alone gives 0.8.
    expect(formatDuration(850)).toBe("0.9 s");
    expect(formatDuration(1_250)).toBe("1.3 s");
    expect(formatDuration(2000)).toBe("2 s");
    expect(formatDuration(9_949)).toBe("9.9 s");
    expect(formatDuration(-5)).toBe("0 s");
  });

  it("rounds to whole seconds, then minutes and seconds, then hours and minutes", () => {
    expect(formatDuration(10 * SECOND)).toBe("10 s");
    expect(formatDuration(59 * SECOND + 499)).toBe("59 s");
    expect(formatDuration(59 * SECOND + 500)).toBe("1 min");
    expect(formatDuration(95 * SECOND)).toBe("1 min 35 s");
    expect(formatDuration(59 * MINUTE + 59 * SECOND)).toBe("59 min 59 s");
    expect(formatDuration(HOUR)).toBe("1 h");
    expect(formatDuration(3_720_000)).toBe("1 h 2 min");
    expect(formatDuration(26 * HOUR + 30 * SECOND)).toBe("26 h");
    expect(formatDuration(26 * HOUR + MINUTE)).toBe("26 h 1 min");
  });
});

describe("counts and costs", () => {
  it("shortens counts to thousands and millions, without a trailing .0", () => {
    expect(compactCount(999)).toBe("999");
    expect(compactCount(1000)).toBe("1k");
    expect(compactCount(1234)).toBe("1.2k");
    expect(compactCount(999_999)).toBe("1000k");
    expect(compactCount(1_000_000)).toBe("1M");
    expect(compactCount(2_500_000)).toBe("2.5M");
  });

  it("names one file and several files", () => {
    expect(filesLabel(0)).toBe("0 files");
    expect(filesLabel(1)).toBe("1 file");
    expect(filesLabel(2)).toBe("2 files");
  });

  it("orders text by code point, whatever the locale", () => {
    expect(["b", "B", "a", "b"].toSorted(compareText)).toEqual(["B", "a", "b", "b"]);
  });

  it("reads an optional field trimmed, and a blank one as not set", () => {
    expect(blankAsNull(" api ")).toBe("api");
    expect(blankAsNull("   ")).toBeNull();
    expect(blankAsNull("")).toBeNull();
  });

  it("lists items in words", () => {
    expect(listInWords([])).toBe("");
    expect(listInWords(["api"])).toBe("api");
    expect(listInWords(["api", "web"])).toBe("api and web");
    expect(listInWords(["api", "web", "docs"])).toBe("api, web and docs");
  });

  it("keeps four decimals for a cost under a dollar, and two otherwise", () => {
    expect(formatCost(0)).toBe("$0.00");
    expect(formatCost(0.0421)).toBe("$0.0421");
    expect(formatCost(1)).toBe("$1.00");
    expect(formatCost(12.345)).toBe("$12.35");
  });
});

describe("clockTime", () => {
  it("says the time of day to the second on a 24-hour clock", () => {
    const afternoon = new Date(2026, 10, 28, 14, 3, 27).getTime();
    expect(clockTime(afternoon)).toMatch(/^14\D03\D27$/);
  });
});
