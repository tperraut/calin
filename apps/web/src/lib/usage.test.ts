import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { makeWindow, row } from "../test/integrations";
import {
  axisCost,
  barHeight,
  dayKey,
  dayLabel,
  fillDays,
  meterState,
  niceScale,
  percentUsed,
  rangeBounds,
  rangeFromQuery,
  refusalText,
  reportedText,
  resetText,
  todayKey,
  windowName,
  windowSummary,
} from "./usage";

const at = (day: number, hour: number, minute = 0): number =>
  new Date(2026, 10, day, hour, minute).getTime();

describe("niceScale", () => {
  it.each([
    [0, 1, [0, 0.5, 1]],
    [-3, 1, [0, 0.5, 1]],
    [Number.NaN, 1, [0, 0.5, 1]],
    [0.0003, 0.0003, [0, 0.0001, 0.0002, 0.0003]],
    [0.004, 0.004, [0, 0.002, 0.004]],
    [0.0041, 0.006, [0, 0.002, 0.004, 0.006]],
    [1, 1, [0, 0.5, 1]],
    [2.6, 3, [0, 1, 2, 3]],
    [7.3, 7.5, [0, 2.5, 5, 7.5]],
    [10, 10, [0, 5, 10]],
    [48.37, 60, [0, 20, 40, 60]],
    [99.99, 100, [0, 50, 100]],
    [101, 150, [0, 50, 100, 150]],
    [12345, 15000, [0, 5000, 10000, 15000]],
  ])("gives %s a top of %s with 3 or 4 even ticks", (max, top, ticks) => {
    const scale = niceScale(max);
    expect(scale.max).toBe(top);
    expect(scale.ticks).toEqual(ticks);
    expect(scale.ticks.length).toBeGreaterThanOrEqual(3);
    expect(scale.ticks.length).toBeLessThanOrEqual(4);
    if (max > 0) expect(scale.max).toBeGreaterThanOrEqual(max);
  });

  it("scales bars to the top of the axis, never past it, and keeps a tiny one visible", () => {
    const scale = niceScale(48.37);
    expect(barHeight(60, scale, 160)).toBe(160);
    expect(barHeight(30, scale, 160)).toBe(80);
    expect(barHeight(48.37, scale, 160)).toBeCloseTo(128.99, 1);
    expect(barHeight(0.0001, scale, 160)).toBe(2);
    expect(barHeight(0, scale, 160)).toBe(0);
    expect(barHeight(500, scale, 160)).toBe(160);
  });

  it("labels ticks with the decimals the step needs", () => {
    expect(niceScale(48.37).ticks.map((tick) => axisCost(tick, 20))).toEqual([
      "$0",
      "$20",
      "$40",
      "$60",
    ]);
    expect([0, 0.5, 1].map((tick) => axisCost(tick, 0.5))).toEqual(["$0.00", "$0.50", "$1.00"]);
    expect([0, 2.5, 5].map((tick) => axisCost(tick, 2.5))).toEqual(["$0.00", "$2.50", "$5.00"]);
    expect(axisCost(0.0002, 0.0001)).toBe("$0.0002");
  });
});

describe("days", () => {
  it("fills the days missing between the first and the last, in order", () => {
    const days = fillDays([row("2026-11-03", 2), row("2026-11-01", 1), row("not a day", 5)]);
    expect(days.map((day) => [day.key, day.totals.cost_usd])).toEqual([
      ["2026-11-01", 1],
      ["2026-11-02", 0],
      ["2026-11-03", 2],
    ]);
    expect(fillDays([])).toEqual([]);
  });

  it("crosses a month end and a clock change", () => {
    const days = fillDays([row("2026-10-24", 1), row("2026-11-02", 1)]);
    expect(days).toHaveLength(10);
    expect(days.at(-1)?.key).toBe("2026-11-02");
  });

  it("marks the server's last day as today when the range ends now, and none otherwise", () => {
    const days = fillDays([row("2026-11-26", 1), row("2026-11-28", 2)]);
    expect(todayKey(days, true)).toBe("2026-11-28");
    expect(todayKey(days, false)).toBeUndefined();
    expect(todayKey([], true)).toBeUndefined();
  });

  it("asks for the last n days, today included, from local midnight", () => {
    const now = at(27, 15, 30);
    const { from, to } = rangeBounds(7, now);
    expect(to).toBe(now);
    expect(from).toBe(new Date(2026, 10, 21).getTime());
    expect(dayKey(from)).toBe("2026-11-21");
  });

  it("reads the range from the query, 30 days otherwise", () => {
    expect(rangeFromQuery("7")).toBe(7);
    expect(rangeFromQuery("90")).toBe(90);
    expect(rangeFromQuery(["30"])).toBe(30);
    expect(rangeFromQuery(undefined)).toBe(30);
    expect(rangeFromQuery("12")).toBe(30);
  });
});

describe("limit windows", () => {
  it("names windows in words, and shows an unknown name as it is", () => {
    expect(windowName("five_hour")).toBe("5-hour window");
    expect(windowName("seven_day")).toBe("7-day window");
    expect(windowName("seven_day_opus")).toBe("7-day window (opus)");
    expect(windowName("burst_quota")).toBe("burst_quota");
    expect(windowName("RateLimit")).toBe("RateLimit");
  });

  it("says how much is used, when it resets and when it was reported", () => {
    const now = at(26, 15); // a Thursday
    expect(percentUsed(0.623)).toBe("62%");
    expect(percentUsed(1.4)).toBe("100%");
    expect(resetText(makeWindow("five_hour", { resets_at: at(26, 18) }), now)).toBe(
      "resets at 18:00",
    );
    expect(resetText(makeWindow("seven_day", { resets_at: at(29, 9) }), now)).toBe(
      "resets Sunday 09:00",
    );
    // Beyond the week, the date in the viewer's locale.
    expect(resetText(makeWindow("seven_day", { resets_at: at(26 + 10, 9) }), now)).toMatch(
      /^resets (6 December|December 6) 09:00$/,
    );
    expect(resetText(makeWindow("five_hour"), now)).toBeUndefined();
    expect(reportedText(now - 3 * 60_000, now)).toBe("reported 3 min ago");
    expect(windowSummary(makeWindow("five_hour", { used: 0.62 }))).toBe("5-hour window 62% used");
  });

  it("calls a refusal out in words, with or without a reset time", () => {
    const now = at(26, 15);
    expect(
      refusalText(makeWindow("five_hour", { refused: true, resets_at: at(26, 18) }), now),
    ).toBe("Requests are being refused until 18:00.");
    expect(refusalText(makeWindow("five_hour", { refused: true }), now)).toBe(
      "Requests are being refused.",
    );
  });

  it("gives every meter state a word", () => {
    expect(meterState(makeWindow("a", { used: 0.2 }))).toEqual({
      label: "Room left",
      tone: "success",
    });
    expect(meterState(makeWindow("a", { used: 0.8 })).label).toBe("Filling up");
    expect(meterState(makeWindow("a", { used: 0.95 })).label).toBe("Nearly full");
    expect(meterState(makeWindow("a", { used: 0.1, refused: true })).label).toBe(
      "Refusing requests",
    );
  });
});

describe("day labels across time zones", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  // Node applies a change of TZ at once: the zones below are a day apart at midnight UTC.
  const zones = [
    "UTC",
    "Pacific/Pago_Pago",
    "Pacific/Kiritimati",
    "America/Los_Angeles",
    "Asia/Tokyo",
  ];

  it("reads a key as the same calendar date in every viewer zone", () => {
    const labels = zones.map((one) => {
      vi.stubEnv("TZ", one);
      return [dayLabel("2026-11-27"), dayLabel("2026-11-27", "long")];
    });
    vi.stubEnv("TZ", "Pacific/Pago_Pago");
    // A sanity check that the zone did change: a naive reading lands on the 26th there.
    expect(new Date("2026-11-27").getDate()).toBe(26);
    for (const [short, long] of labels) {
      expect(short).toBe(labels[0]?.[0]);
      expect(long).toBe(labels[0]?.[1]);
      expect(short).toMatch(/27/);
      expect(long).toMatch(/Friday/);
      expect(long).toMatch(/November/);
    }
  });

  it("fills days the same way in every viewer zone, across a clock change", () => {
    for (const one of zones) {
      vi.stubEnv("TZ", one);
      const days = fillDays([row("2026-10-24", 1), row("2026-11-02", 1)]);
      expect(days.map((day) => day.key)).toEqual([
        "2026-10-24",
        "2026-10-25",
        "2026-10-26",
        "2026-10-27",
        "2026-10-28",
        "2026-10-29",
        "2026-10-30",
        "2026-10-31",
        "2026-11-01",
        "2026-11-02",
      ]);
    }
  });
});
