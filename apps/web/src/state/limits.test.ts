import { flush } from "solid-js";
import { describe, expect, it, vi } from "vite-plus/test";
import type { RunnerLimits } from "../gen/RunnerLimits";
import {
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  settle,
} from "../test/fakes";
import { makeLimits, makeWindow } from "../test/integrations";
import { makeAgent } from "../test/orchestration";
import { createAppState } from "./app";
import { closestToFull, mergeAllLimits, mergeLimits, refusingWindow } from "./limits";
import { applyEvent, emptySessionView } from "./sessionReducer";
import { createSessionStore } from "./sessionStore";

describe("limits", () => {
  it("keeps the newest report per runner, sorted by runner, and ignores an older one", () => {
    const a1 = makeLimits("mock", [makeWindow("five_hour", { used: 0.1 })], 100);
    const b = makeLimits("claude", [makeWindow("five_hour", { used: 0.5 })], 200);
    const a2 = makeLimits("mock", [makeWindow("five_hour", { used: 0.3 })], 300);
    let list = mergeLimits([], a1);
    list = mergeLimits(list, b);
    expect(list.map((one) => one.runner)).toEqual(["claude", "mock"]);
    list = mergeLimits(list, a2);
    expect(list.find((one) => one.runner === "mock")).toBe(a2);
    expect(mergeLimits(list, a1)).toBe(list);
    expect(mergeAllLimits(list, [a1, b])).toEqual(list);
  });

  it("finds the window closest to full and the window refusing requests", () => {
    const list: RunnerLimits[] = [
      makeLimits("claude", [
        makeWindow("five_hour", { used: 0.62 }),
        makeWindow("seven_day", { used: 0.9, refused: true, resets_at: 5000 }),
        makeWindow("opus", { used: 0.3, refused: true, resets_at: 9000 }),
      ]),
      makeLimits("mock", [makeWindow("five_hour", { used: 0.95 })]),
    ];
    expect(closestToFull(list)?.used).toBe(0.95);
    expect(closestToFull([])).toBeUndefined();
    expect(refusingWindow(list, "claude")?.name).toBe("opus");
    expect(refusingWindow(list, "mock")).toBeUndefined();
    expect(refusingWindow(list, undefined)).toBeUndefined();
  });

  it("takes a report as new as the one held: it is no older", () => {
    const held = makeLimits("mock", [makeWindow("five_hour", { used: 0.1 })], 100);
    const same = makeLimits("mock", [makeWindow("five_hour", { used: 0.4 })], 100);
    expect(mergeLimits([held], same)).toEqual([same]);
  });

  it("keeps the first window found when two are as full, or reset as late", () => {
    const list: RunnerLimits[] = [
      makeLimits("claude", [
        makeWindow("five_hour", { used: 0.5, refused: true, resets_at: 9000 }),
        makeWindow("seven_day", { used: 0.5, refused: true, resets_at: 9000 }),
      ]),
    ];
    expect(closestToFull(list)?.name).toBe("five_hour");
    expect(refusingWindow(list, "claude")?.name).toBe("five_hour");
  });

  it("takes a refusing window with no reset time as the one resetting last", () => {
    const list: RunnerLimits[] = [
      makeLimits("claude", [
        makeWindow("five_hour", { refused: true, resets_at: 5000 }),
        makeWindow("opus", { refused: true, resets_at: null }),
        makeWindow("seven_day", { refused: true, resets_at: 9000 }),
      ]),
    ];
    expect(refusingWindow(list, "claude")?.name).toBe("opus");
  });

  it("adds no transcript row for a limits event", () => {
    const { view, outcome } = applyEvent(
      emptySessionView,
      makeEvent(1, { type: "limits", windows: [makeWindow("five_hour")] }),
    );
    expect(outcome).toBe("applied");
    expect(view.transcript).toEqual([]);
    expect(view.seq).toBe(1);
  });

  it("reports a limits event with the runner of the agent that emitted it", async () => {
    const socket = new FakeSocket();
    const onLimits = vi.fn();
    const api = fakeApi({
      getSession: () => Promise.resolve(makeDetail(makeSession("s1", 1000, { runner: "mock" }))),
    });
    const store = createSessionStore({ api, socket, schedule: immediately, onLimits }, "s1");
    const stop = store.start();
    await settle();
    flush();
    const windows = [makeWindow("five_hour", { used: 0.4 })];
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(1, { type: "agent_spawned", agent: makeAgent("a1", { runner: "claude" }) }),
    });
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(2, { type: "limits", windows }, "a1"),
    });
    // An agent the session does not know: the session's own runner.
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(3, { type: "limits", windows }, "ghost"),
    });
    expect(onLimits.mock.calls).toEqual([
      [{ runner: "claude", windows, reported_at: 1002 }],
      [{ runner: "mock", windows, reported_at: 1003 }],
    ]);
    stop();
  });

  it("seeds the app's limits from the server, then takes newer ones from session events", async () => {
    const socket = new FakeSocket();
    const seeded = makeLimits("claude", [makeWindow("five_hour", { used: 0.2 })], 1002);
    const api = fakeApi({
      health: () => Promise.reject(new Error("no")),
      getSession: () => Promise.resolve(makeDetail(makeSession("s1", 1000, { runner: "claude" }))),
      getUsageLimits: () => Promise.resolve([seeded]),
    });
    const app = createAppState({ api, socket, schedule: immediately });
    const stop = app.start();
    await settle();
    socket.setState("open");
    await settle();
    flush();
    expect(app.limits()).toEqual([seeded]);

    const store = app.openSession("s1");
    const stopStore = store.start();
    await settle();
    // Older than what the server said: ignored.
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(1, { type: "limits", windows: [makeWindow("five_hour", { used: 0.1 })] }),
    });
    flush();
    expect(app.limits()).toEqual([seeded]);
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(2, { type: "limits", windows: [makeWindow("five_hour", { used: 0.7 })] }),
    });
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(3, { type: "limits", windows: [makeWindow("five_hour", { used: 0.8 })] }),
    });
    flush();
    expect(app.limits()).toEqual([
      { runner: "claude", windows: [makeWindow("five_hour", { used: 0.8 })], reported_at: 1003 },
    ]);
    stopStore();
    stop();
  });
});
