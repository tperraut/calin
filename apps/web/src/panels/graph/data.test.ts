import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { Graph } from "../../gen/Graph";
import type { Timeline } from "../../gen/Timeline";
import { conflict, deferred } from "../../test/fakes";
import { makeGraph, makeTimeline, T0 } from "../../test/orchestration";
import {
  createOrchestrationData,
  PLAY_MAX_STEP_MS,
  PLAY_MIN_STEP_MS,
  PLAY_TICK_MS,
  playStep,
  REFETCH_DELAY_MS,
  SCRUB_DELAY_MS,
} from "./data";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

/** Lets resolved promises run, then the signals follow. */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
  flush();
}

function setup(
  answer: (at?: number) => Promise<Graph> = (at) =>
    Promise.resolve(makeGraph({ at: at ?? T0 + 60_000 })),
) {
  const graph = vi.fn(answer);
  const timeline = vi.fn<() => Promise<Timeline>>(() => Promise.resolve(makeTimeline()));
  const data = createOrchestrationData({ graph, timeline });
  return { data, graph, timeline };
}

describe("orchestration data", () => {
  it("reads the graph when the panel is first seen, not before", async () => {
    const { data, graph } = setup();
    await settle();
    expect(graph).not.toHaveBeenCalled();
    data.setVisible(true);
    await settle();
    expect(graph.mock.calls).toEqual([[undefined]]);
    expect(data.graph()?.nodes).toHaveLength(10);
    expect(data.live()).toBe(true);
  });

  it("gathers a burst of changes into one read, about 400 ms after the last one", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    for (let burst = 0; burst < 5; burst += 1) {
      data.changed();
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(graph).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS - 101);
    expect(graph).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(graph).toHaveBeenCalledTimes(2);
  });

  it("reads nothing while hidden, and once on the way back if something changed meanwhile", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.setVisible(false);
    data.changed();
    data.changed();
    await vi.advanceTimersByTimeAsync(5000);
    expect(graph).toHaveBeenCalledTimes(1);
    data.setVisible(true);
    await settle();
    expect(graph).toHaveBeenCalledTimes(2);
    // Nothing changed while hidden this time: coming back costs nothing.
    data.setVisible(false);
    data.setVisible(true);
    await vi.advanceTimersByTimeAsync(5000);
    expect(graph).toHaveBeenCalledTimes(2);
  });

  it("drops a pending read when the panel is hidden before the delay is over", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS / 2);
    data.setVisible(false);
    await vi.advanceTimersByTimeAsync(5000);
    expect(graph).toHaveBeenCalledTimes(1);
    data.setVisible(true);
    await settle();
    expect(graph).toHaveBeenCalledTimes(2);
  });

  it("keeps the graph on screen while another read is in flight, and after a failed one", async () => {
    const second = deferred<Graph>();
    const answers = [
      () => Promise.resolve(makeGraph()),
      () => second.promise,
      () => Promise.reject<Graph>(conflict("the log is being rebuilt")),
    ];
    const { data } = setup(() => (answers.shift() ?? (() => Promise.resolve(makeGraph())))());
    data.setVisible(true);
    await settle();
    const first = data.graph();
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS);
    flush();
    expect(data.refreshing()).toBe(true);
    expect(data.graph()).toBe(first);
    second.resolve(makeGraph({ to_ts: T0 + 90_000 }));
    await settle();
    expect(data.refreshing()).toBe(false);
    expect(data.graph()?.to_ts).toBe(T0 + 90_000);

    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS);
    await settle();
    expect(data.graphError()).toBe("the log is being rebuilt");
    expect(data.graph()?.to_ts).toBe(T0 + 90_000);
    data.retry();
    await settle();
    expect(data.graphError()).toBeUndefined();
  });

  it("says why the first read failed", async () => {
    const { data } = setup(() => Promise.reject(conflict("session is being deleted")));
    data.setVisible(true);
    await settle();
    expect(data.graph()).toBeUndefined();
    expect(data.graphError()).toBe("session is being deleted");
  });

  it("asks for the graph at the moment the slider rests on, and stops following the present", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 10_000);
    data.scrub(T0 + 20_000);
    data.scrub(T0 + 30_000);
    flush();
    expect(data.live()).toBe(false);
    expect(data.at()).toBe(T0 + 30_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    flush();
    expect(graph.mock.calls).toEqual([[undefined], [T0 + 30_000]]);
    expect(data.graph()?.at).toBe(T0 + 30_000);
    // New events do not change the past: nothing is read for them.
    data.changed();
    await vi.advanceTimersByTimeAsync(2000);
    expect(graph).toHaveBeenCalledTimes(2);
  });

  it("shows only the answer to the newest question when answers arrive out of order", async () => {
    const slow = deferred<Graph>();
    const { data } = setup((at) =>
      at === T0 + 10_000 ? slow.promise : Promise.resolve(makeGraph({ at: at ?? T0 + 60_000 })),
    );
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 10_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    data.scrub(T0 + 40_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    flush();
    expect(data.graph()?.at).toBe(T0 + 40_000);
    slow.resolve(makeGraph({ at: T0 + 10_000 }));
    await settle();
    expect(data.graph()?.at).toBe(T0 + 40_000);
  });

  it("goes back to the present with Live, reading it at once, and freezes with it off", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 10_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    data.setLive(true);
    await settle();
    expect(data.live()).toBe(true);
    expect(data.at()).toBeUndefined();
    expect(graph.mock.calls.at(-1)).toEqual([undefined]);
    // Events are followed again.
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS);
    expect(graph).toHaveBeenCalledTimes(4);

    data.setLive(false);
    flush();
    expect(data.live()).toBe(false);
    expect(data.at()).toBe(T0 + 60_000);
    data.changed();
    await vi.advanceTimersByTimeAsync(2000);
    expect(graph).toHaveBeenCalledTimes(4);
  });

  it("replays from the slider's moment to the present in steps, then follows it live", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 54_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    graph.mockClear();
    data.play();
    await settle();
    expect(data.playing()).toBe(true);
    expect(graph.mock.calls).toEqual([[T0 + 54_000]]);
    // A minute of history in 40 steps: 1.5 s of it per tick.
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    flush();
    expect(graph.mock.calls.map(([at]) => at)).toEqual([T0 + 54_000, T0 + 55_500, T0 + 57_000]);
    expect(data.at()).toBe(T0 + 57_000);
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    await settle();
    // The step that would pass the newest event ends the replay.
    expect(data.playing()).toBe(false);
    expect(data.live()).toBe(true);
    expect(graph.mock.calls.at(-1)).toEqual([undefined]);
    const calls = graph.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5 * PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(calls);
  });

  it("starts a replay from the beginning when asked from the present, and pauses where it is", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.play();
    await settle();
    expect(data.live()).toBe(false);
    expect(data.at()).toBe(T0);
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    flush();
    expect(data.at()).toBe(T0 + 1500);
    data.pause();
    flush();
    expect(data.playing()).toBe(false);
    const calls = graph.mock.calls.length;
    await vi.advanceTimersByTimeAsync(10 * PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(calls);
    expect(data.at()).toBe(T0 + 1500);
    expect(data.live()).toBe(false);
  });

  it("starts a replay from the beginning when frozen at the present", async () => {
    const { data } = setup();
    data.setVisible(true);
    await settle();
    data.setLive(false);
    data.play();
    flush();
    expect(data.at()).toBe(T0);
  });

  it("holds the replay while an answer is on its way, and while hidden", async () => {
    const slow = deferred<Graph>();
    let calls = 0;
    const { data, graph } = setup(() => {
      calls += 1;
      return calls === 3 ? slow.promise : Promise.resolve(makeGraph());
    });
    data.setVisible(true);
    await settle();
    data.play();
    await settle();
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(4 * PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(3);
    slow.resolve(makeGraph());
    await settle();
    data.setVisible(false);
    await vi.advanceTimersByTimeAsync(4 * PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(3);
    data.setVisible(true);
    await vi.advanceTimersByTimeAsync(PLAY_TICK_MS);
    expect(graph).toHaveBeenCalledTimes(4);
  });

  it("reads the timeline only once it has been shown, then keeps it current while it is", async () => {
    const { data, timeline } = setup();
    data.setVisible(true);
    await settle();
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS);
    expect(timeline).not.toHaveBeenCalled();
    data.setView("timeline");
    await settle();
    expect(timeline).toHaveBeenCalledTimes(1);
    expect(data.timeline()?.lanes).toHaveLength(3);
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS);
    expect(timeline).toHaveBeenCalledTimes(2);
    // Behind the graph it waits; back in view it catches up.
    data.setView("graph");
    data.changed();
    await vi.advanceTimersByTimeAsync(REFETCH_DELAY_MS * 3);
    expect(timeline).toHaveBeenCalledTimes(2);
    data.setView("timeline");
    await settle();
    expect(timeline).toHaveBeenCalledTimes(3);
  });

  it("reads nothing more for the timeline chosen again while on screen", async () => {
    const { data, timeline } = setup();
    data.setVisible(true);
    data.setView("timeline");
    await settle();
    data.setView("timeline");
    await settle();
    expect(timeline).toHaveBeenCalledTimes(1);
  });

  it("is refreshing while only the timeline is on its way", async () => {
    const answer = deferred<Timeline>();
    const data = createOrchestrationData({
      graph: () => Promise.resolve(makeGraph()),
      timeline: () => answer.promise,
    });
    data.setVisible(true);
    data.setView("timeline");
    await settle();
    expect(data.graph()).toBeDefined();
    expect(data.refreshing()).toBe(true);
    answer.resolve(makeTimeline());
    await settle();
    expect(data.refreshing()).toBe(false);
  });

  it("reads nothing more once disposed", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.changed();
    data.dispose();
    await vi.advanceTimersByTimeAsync(5000);
    expect(graph).toHaveBeenCalledTimes(1);
  });

  it("drops what answers after it is disposed, failures included", async () => {
    const graphAnswer = deferred<Graph>();
    const timelineAnswer = deferred<Timeline>();
    const graph = vi.fn(() => graphAnswer.promise);
    const timeline = vi.fn(() => timelineAnswer.promise);
    const data = createOrchestrationData({ graph, timeline });
    data.setVisible(true);
    data.setView("timeline");
    await settle();
    expect(data.refreshing()).toBe(true);
    data.dispose();
    graphAnswer.reject(conflict("too late"));
    timelineAnswer.resolve(makeTimeline());
    await settle();
    expect(data.graphError()).toBeUndefined();
    expect(data.timeline()).toBeUndefined();
    expect(data.refreshing()).toBe(false);
  });

  it("says why the timeline could not be read, and reads it again on retry", async () => {
    const answers = [() => Promise.reject<Timeline>(conflict("no timeline yet"))];
    const timeline = vi.fn(() => (answers.shift() ?? (() => Promise.resolve(makeTimeline())))());
    const data = createOrchestrationData({
      graph: () => Promise.resolve(makeGraph()),
      timeline,
    });
    data.setVisible(true);
    data.setView("timeline");
    await settle();
    expect(data.timelineError()).toBe("no timeline yet");
    data.retry();
    await settle();
    expect(timeline).toHaveBeenCalledTimes(2);
    expect(data.timelineError()).toBeUndefined();
    expect(data.timeline()?.lanes).toHaveLength(3);
  });

  it("shows no failure of a question that a newer one replaced", async () => {
    const old = deferred<Graph>();
    const { data } = setup((at) =>
      at === T0 + 10_000 ? old.promise : Promise.resolve(makeGraph({ at: at ?? T0 + 60_000 })),
    );
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 10_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    data.scrub(T0 + 40_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    old.reject(conflict("stale"));
    await settle();
    expect(data.graphError()).toBeUndefined();
    expect(data.graph()?.at).toBe(T0 + 40_000);
  });

  it("on retry in the past, reads the moment on screen again", async () => {
    const answers = [
      () => Promise.resolve(makeGraph()),
      () => Promise.reject<Graph>(conflict("busy")),
    ];
    const { data, graph } = setup((at) =>
      (answers.shift() ?? (() => Promise.resolve(makeGraph({ at }))))(),
    );
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 20_000);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS);
    await settle();
    expect(data.graphError()).toBe("busy");
    data.retry();
    await settle();
    expect(graph.mock.calls.at(-1)).toEqual([T0 + 20_000]);
    expect(data.graphError()).toBeUndefined();
    expect(data.live()).toBe(false);
  });

  it("reads nothing for a slider that rests while the panel is hidden", async () => {
    const { data, graph } = setup();
    data.setVisible(true);
    await settle();
    data.scrub(T0 + 20_000);
    data.setVisible(false);
    await vi.advanceTimersByTimeAsync(SCRUB_DELAY_MS * 2);
    expect(graph).toHaveBeenCalledTimes(1);
    expect(data.at()).toBe(T0 + 20_000);
  });

  it("freezes at the present before any graph came, and only once", () => {
    vi.setSystemTime(T0 + 99_000);
    const { data } = setup(() => new Promise<Graph>(() => {}));
    data.setLive(false);
    flush();
    expect(data.live()).toBe(false);
    expect(data.at()).toBe(T0 + 99_000);
    vi.setSystemTime(T0 + 120_000);
    data.setLive(false);
    flush();
    expect(data.at()).toBe(T0 + 99_000);
  });

  it("has nothing to replay before any graph came, and starts one replay at a time", async () => {
    const { data, graph } = setup();
    data.play();
    flush();
    expect(data.playing()).toBe(false);
    data.setVisible(true);
    await settle();
    data.play();
    await settle();
    const calls = graph.mock.calls.length;
    data.play();
    await settle();
    expect(graph).toHaveBeenCalledTimes(calls);
    expect(data.at()).toBe(T0);
  });
});

describe("playStep", () => {
  it("covers the whole history in about 20 seconds, within bounds per step", () => {
    expect(playStep(60_000)).toBe(1500);
    expect(playStep(2000)).toBe(PLAY_MIN_STEP_MS);
    expect(playStep(48 * 3_600_000)).toBe(PLAY_MAX_STEP_MS);
  });
});
