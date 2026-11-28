import { describe, expect, it } from "vite-plus/test";
import { makeSpan, makeSubagent, makeTimeline, T0 } from "../../test/orchestration";
import {
  arrowBox,
  arrowMarker,
  arrowSentence,
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
  nestSpans,
  spanBox,
  spanDuration,
  spanLabel,
  spanStatus,
  timelineEnd,
  timeToX,
  toolSeries,
  visibleSpans,
  xToTime,
} from "./timelineModel";

const titles: Record<string, string> = { s1: "Ship the health check", s2: "API: add /health" };
const lanes = () => laneViews(makeTimeline(), "s1", (id) => titles[id]);

describe("time to pixels", () => {
  it("maps the start to 0 and scales by the zoom, both ways", () => {
    const scale = { from: T0, pxPerMs: 0.01 };
    expect(timeToX(scale, T0)).toBe(0);
    expect(timeToX(scale, T0 + 60_000)).toBe(600);
    expect(xToTime(scale, 150)).toBe(T0 + 15_000);
    expect(xToTime(scale, timeToX(scale, T0 + 1234))).toBeCloseTo(T0 + 1234);
  });

  it("fits the whole history in the width given, and never divides by nothing", () => {
    const scale = fitScale(T0, T0 + 60_000, 300);
    expect(timeToX(scale, T0 + 60_000)).toBeCloseTo(300);
    expect(Number.isFinite(fitScale(T0, T0, 300).pxPerMs)).toBe(true);
    // Before the chart is measured it is a pixel wide, never zero.
    expect(fitScale(T0, T0 + 2000, 0).pxPerMs).toBe(1 / 2000);
  });

  it("ends the axis at the newest event, or at now while a span is open", () => {
    const timeline = makeTimeline();
    expect(timelineEnd(timeline, T0 + 90_000)).toBe(T0 + 90_000);
    expect(timelineEnd(timeline, T0 + 1000)).toBe(T0 + 60_000);
    const closed = makeTimeline({ lanes: [timeline.lanes[0]] });
    expect(timelineEnd(closed, T0 + 90_000)).toBe(T0 + 60_000);
  });

  it("puts ticks on round moments, a readable distance apart", () => {
    const ticks = axisTicks({ from: T0, pxPerMs: 0.01 }, 0, 600);
    // 90px at 0.01 px/ms is 9 s: the next round step is 10 s.
    expect(ticks.map((tick) => tick.ts - T0)).toEqual([
      0, 10_000, 20_000, 30_000, 40_000, 50_000, 60_000,
    ]);
    expect(ticks[1].x).toBe(100);
  });

  it("takes a step that is exactly 90 px", () => {
    const ticks = axisTicks({ from: T0, pxPerMs: 0.09 }, 0, 200);
    expect(ticks.map((tick) => tick.ts - T0)).toEqual([0, 1000, 2000]);
  });

  it("puts a tick a day apart past the longest step, and never more than 200", () => {
    const zoomedOut = axisTicks({ from: 0, pxPerMs: 90 / 100_000_000 }, 0, 160);
    expect(zoomedOut.map((tick) => tick.ts)).toEqual([0, 86_400_000, 172_800_000]);
    expect(axisTicks({ from: T0, pxPerMs: 0.9 }, 0, 1_000_000)).toHaveLength(200);
  });

  it("draws a margin of 240 px on each side of the viewport", () => {
    expect(cullRange(1000, 500)).toEqual({ x0: 760, x1: 1740 });
  });
});

describe("spans", () => {
  const scale = { from: T0, pxPerMs: 0.01 };

  it("places a closed span from its start to its end", () => {
    expect(
      spanBox(makeSpan("a", { start: T0 + 5000, end: T0 + 17_000 }), scale, T0 + 99_000),
    ).toEqual({
      x: 50,
      w: 120,
      open: false,
    });
  });

  it("extends an open span to now, and lets it grow with the clock", () => {
    const open = makeSpan("a", { start: T0 + 10_000, end: null });
    expect(spanBox(open, scale, T0 + 60_000)).toEqual({ x: 100, w: 500, open: true });
    expect(spanBox(open, scale, T0 + 70_000).w).toBe(600);
    // A clock behind the span's start still leaves a mark.
    expect(spanBox(open, scale, T0).w).toBe(2);
  });

  it("gives an instant a visible width", () => {
    expect(spanBox(makeSpan("a", { start: T0, end: T0 }), scale, T0).w).toBe(2);
  });

  it("nests tool and waiting spans in the turn they lie in", () => {
    const spans = [
      makeSpan("turn-1", { start: T0, end: T0 + 10_000 }),
      makeSpan("t1", { kind: "tool", start: T0 + 1000, end: T0 + 2000 }),
      makeSpan("w1", { kind: "waiting", start: T0 + 3000, end: T0 + 4000 }),
      makeSpan("turn-2", { start: T0 + 20_000, end: null }),
      makeSpan("t2", { kind: "tool", start: T0 + 25_000, end: null }),
      makeSpan("stray", { kind: "tool", start: T0 + 15_000, end: T0 + 16_000 }),
    ];
    expect(
      nestSpans(spans).map((group) => [group.turn?.id, group.inner.map((span) => span.id)]),
    ).toEqual([
      ["turn-1", ["t1", "w1"]],
      ["turn-2", ["t2"]],
      [undefined, ["stray"]],
    ]);
  });

  it("nests a span that starts on a turn's first or last moment in that turn", () => {
    const spans = [
      makeSpan("turn-1", { start: T0, end: T0 + 10_000 }),
      makeSpan("at-start", { kind: "tool", start: T0 }),
      makeSpan("at-end", { kind: "tool", start: T0 + 10_000 }),
    ];
    expect(nestSpans(spans).map((group) => group.inner.map((span) => span.id))).toEqual([
      ["at-start", "at-end"],
    ]);
  });

  it("lists the drawn spans of every lane, turns under the rest", () => {
    const drawn = drawnSpans(lanes(), scale, T0 + 60_000, { x0: 0, x1: 10_000 });
    expect(drawn.map((one) => [one.key, one.row])).toEqual([
      ["a1/turn-1", 0],
      ["sub1/turn-sub1", 1],
      ["a2/turn-a2", 2],
      ["a1/wait-1", 0],
      ["a1/t1", 0],
      ["a2/c1", 2],
    ]);
    expect(drawn[4].box).toEqual({ x: 50, w: 120, open: false });
  });

  it("keeps a span that ends exactly at the left edge of the range", () => {
    const span = makeSpan("a", { start: T0, end: T0 + 10_000 });
    expect(visibleSpans([span], scale, T0, 100, 200)).toEqual([span]);
  });

  it("measures how long a span lasted, or has lasted so far, never below zero", () => {
    expect(spanDuration(makeSpan("a", { start: T0, end: T0 + 3000 }), T0 + 9000)).toBe(3000);
    expect(spanDuration(makeSpan("a", { start: T0, end: null }), T0 + 9000)).toBe(9000);
    expect(spanDuration(makeSpan("a", { start: T0, end: T0 }), T0)).toBe(0);
    expect(spanDuration(makeSpan("a", { start: T0, end: null }), T0 - 5000)).toBe(0);
  });

  it("culls the spans outside the viewport, keeping those that cross its edges", () => {
    const spans = [
      makeSpan("before", { start: T0, end: T0 + 5000 }),
      makeSpan("crossing-in", { start: T0 + 8000, end: T0 + 12_000 }),
      makeSpan("inside", { start: T0 + 15_000, end: T0 + 16_000 }),
      makeSpan("crossing-out", { start: T0 + 19_000, end: T0 + 30_000 }),
      makeSpan("after", { start: T0 + 40_000, end: T0 + 50_000 }),
      makeSpan("open", { start: T0 + 2000, end: null }),
    ];
    const held = visibleSpans(spans, scale, T0 + 60_000, 100, 200).map((span) => span.id);
    expect(held).toEqual(["crossing-in", "inside", "crossing-out", "open"]);
    // Hundreds of spans, a narrow window: only what is in it.
    const many = Array.from({ length: 600 }, (_, index) =>
      makeSpan(`s${index}`, { start: T0 + index * 1000, end: T0 + index * 1000 + 800 }),
    );
    expect(visibleSpans(many, { from: T0, pxPerMs: 0.1 }, T0, 1000, 1400)).toHaveLength(5);
  });

  it("labels a tool span only when the text fits, cut to the room there is", () => {
    expect(spanLabel("cargo test", 30)).toBeUndefined();
    expect(spanLabel("cargo test", 200)).toBe("cargo test");
    expect(spanLabel("cargo test --workspace --all-features", 80)).toBe("cargo test -…");
    // 40 px is wide enough, and holds six letters uncut.
    expect(spanLabel("cargo", 39)).toBeUndefined();
    expect(spanLabel("abcdef", 40)).toBe("abcdef");
    expect(spanLabel("abcdefg", 40)).toBe("abcde…");
  });

  it("folds tool kinds into a few series, and words a span's status", () => {
    expect([
      toolSeries("read"),
      toolSeries("search"),
      toolSeries("edit"),
      toolSeries("execute"),
      toolSeries("task"),
      toolSeries(null),
    ]).toEqual(["read", "read", "write", "run", "task", "other"]);
    expect(spanStatus(makeSpan("a", { kind: "tool", status: "failed" }))).toBe("Failed");
    expect(spanStatus(makeSpan("a", { end: null }))).toBe("In progress");
    expect(spanStatus(makeSpan("a"))).toBe("Ended");
  });
});

describe("lanes and arrows", () => {
  it("keeps the server's order, with depth from the spawn tree and a label per agent", () => {
    expect(lanes().map((lane) => [lane.id, lane.depth, lane.label, lane.own])).toEqual([
      ["a1", 0, "lead · Ship the health check", true],
      ["sub1", 1, "explorer", true],
      ["a2", 1, "lead · API: add /health", false],
    ]);
    // A lead whose session's title is not known is just its role.
    expect(laneViews(makeTimeline(), "s1", () => undefined)[2].label).toBe("lead");
    // Sibling sessions share their start: the label keeps the end that tells them apart.
    const long = laneViews(makeTimeline(), "s1", () => "Ship the health check: part B")[2];
    expect(long.short).toBe("lead · Ship the he…ck: part B");
    expect(long.label).toBe("lead · Ship the health check: part B");
  });

  it("counts the depth of every level of the spawn tree", () => {
    const [lead, sub] = makeTimeline().lanes;
    const grandchild = { ...sub, agent: makeSubagent("sub2", "reader"), parent_id: "agent:sub1" };
    const timeline = makeTimeline({ lanes: [lead, sub, grandchild] });
    expect(laneViews(timeline, "s1", () => undefined).map((lane) => lane.depth)).toEqual([0, 1, 2]);
  });

  it("starts the tab stop at the first span of the first lane that has one", () => {
    const [lead, sub, child] = lanes();
    const empty = { ...lead, lane: { ...lead.lane, spans: [] } };
    expect(firstKey([empty, child, sub])).toBe("a2/turn-a2");
    expect(firstKey([empty])).toBeUndefined();
  });

  it("names an agent by its lane's label, the user as you, any other as an agent", () => {
    expect(laneName(lanes(), "sub1")).toBe("explorer");
    expect(laneName(lanes(), null)).toBe("you");
    expect(laneName(lanes(), "gone")).toBe("an agent");
  });

  it("draws the arrows within the range, edges included, and none naming an agent with no lane", () => {
    const index = new Map(lanes().map((lane, at) => [lane.id, at]));
    const scale = { from: T0, pxPerMs: 0.01 };
    const arrows = makeTimeline().arrows;
    // At 0, 20, 100 and 550 px.
    const drawn = (x0: number, x1: number) =>
      drawnArrows(arrows, index, scale, { x0, x1 }).map((one) => one.arrow.id);
    expect(drawn(20, 100)).toEqual(["ar2", "ar3"]);
    expect(drawn(-100, 1000)).toEqual(arrows.map((arrow) => arrow.id));
    const lost = [{ ...arrows[1], to_agent_id: "nobody" }];
    expect(drawnArrows(lost, index, scale, { x0: -100, x1: 1000 })).toEqual([]);
  });

  it("words an arrow with its label, or without one when it has none", () => {
    const [fromUser, spawn] = makeTimeline().arrows;
    const nameOf = (agentId: string | null) => agentId ?? "you";
    expect(arrowSentence(fromUser, nameOf)).toMatch(
      /^\d\d:\d\d:\d\d, Note from you to a1: Ship it$/,
    );
    expect(arrowSentence({ ...spawn, label: null }, nameOf)).toMatch(
      /, Spawned subagent from a1 to sub1$/,
    );
    expect(arrowSentence({ ...spawn, label: "" }, nameOf)).toMatch(/to sub1$/);
  });

  it("draws an arrow at its moment from the sender's lane to the recipient's", () => {
    const index = new Map(lanes().map((lane, at) => [lane.id, at]));
    const scale = { from: T0, pxPerMs: 0.01 };
    const [fromUser, spawn, dispatch, report] = makeTimeline().arrows;
    const middle = (row: number) => row * LANE_HEIGHT + LANE_HEIGHT / 2;
    expect(arrowBox(spawn, index, scale)).toEqual({ x: 20, y1: middle(0), y2: middle(1) });
    expect(arrowBox(dispatch, index, scale)).toEqual({ x: 100, y1: middle(0), y2: middle(2) });
    expect(arrowBox(report, index, scale)).toEqual({ x: 550, y1: middle(2), y2: middle(0) });
    // The user has no lane: the arrow comes from the axis.
    expect(arrowBox(fromUser, index, scale)).toEqual({ x: 0, y1: -6, y2: middle(0) });
    expect(arrowBox({ ...spawn, to_agent_id: "nobody" }, index, scale)).toBeUndefined();
  });

  it("marks each kind of arrow with its own shape", () => {
    const shapes = (["dispatched", "spawned", "message", "report"] as const).map(arrowMarker);
    expect(shapes).toEqual(["triangle", "circle", "diamond", "square"]);
  });

  it("describes each lane as text: turns with what lies inside them", () => {
    const [lead, , child] = describeTimeline(lanes(), T0 + 60_000);
    expect(lead.label).toBe("lead · Ship the health check");
    expect(lead.turns).toHaveLength(1);
    expect(lead.turns[0].text).toMatch(/^Ship the health check, \d\d:\d\d:\d\d, 40 s$/);
    expect(lead.turns[0].inner.map((span) => span.text.replace(/\d\d:\d\d:\d\d/, "T"))).toEqual([
      "Tool call: cargo test --workspace, T, 12 s, Done",
      "Waiting for you: Question: header or footer?, T, 10 s",
    ]);
    expect(child.turns[0].text).toMatch(/50 s so far$/);
  });

  it("describes the spans outside every turn after the turns", () => {
    const [lead] = lanes();
    const stray = makeSpan("stray", { kind: "tool", start: T0 + 50_000, end: T0 + 51_000 });
    const withStray = { ...lead, lane: { ...lead.lane, spans: [...lead.lane.spans, stray] } };
    const [outline] = describeTimeline([withStray], T0 + 60_000);
    expect(outline.turns.map((turn) => [turn.id, turn.text])).toEqual([
      ["turn-1", expect.stringMatching(/^Ship the health check/)],
      ["loose-1", "Outside any turn"],
    ]);
  });
});
