import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Health } from "../gen/Health";
import {
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeSession,
  settle,
  unreachable,
} from "../test/fakes";
import { createAppState } from "./app";

const health: Health = { name: "calin", version: "0.1.0", dry_run: true, runners: ["mock"] };

const stops: Array<() => void> = [];

function setup(routes: Partial<Api> = {}) {
  const socket = new FakeSocket();
  const api = fakeApi({ health: () => Promise.resolve(health), ...routes });
  const app = createAppState({ api, socket, schedule: immediately });
  stops.push(app.start());
  const upsert = (...args: Parameters<typeof makeSession>) =>
    socket.emit("sessions", { op: "session_upserted", session: makeSession(...args) });
  const titles = () => {
    flush();
    return app.sessions().map((session) => session.title);
  };
  return { socket, app, upsert, titles };
}

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
});

describe("app state", () => {
  it("lists sessions from the sessions topic, newest first", () => {
    const { socket, app, upsert, titles } = setup();
    expect(socket.subscriberCount("sessions")).toBe(1);

    upsert("a", 100);
    upsert("c", 300);
    upsert("b", 200);
    expect(titles()).toEqual(["Session c", "Session b", "Session a"]);
    expect(app.sessionsLoaded()).toBe(false);

    socket.emit("sessions", { op: "subscribed", topic: "sessions", head_seq: 0 });
    flush();
    expect(app.sessionsLoaded()).toBe(true);
  });

  it("updates a listed session in place", () => {
    const { app, upsert, titles } = setup();
    upsert("a", 100);
    upsert("b", 200);

    upsert("a", 100, { status: "running", title: "Renamed" });

    expect(titles()).toEqual(["Session b", "Renamed"]);
    expect(app.sessions()[1].status).toBe("running");
  });

  it("adds a created session to the list before the socket reports it", async () => {
    const created = makeSession("new", 500, { title: "Fresh" });
    const createSession = vi.fn<Api["createSession"]>().mockResolvedValue(makeDetail(created));
    const { app, upsert, titles } = setup({ createSession });
    upsert("old", 100);

    await expect(app.createSession({ prompt: "go", runner: "mock" })).resolves.toBe(created);

    expect(createSession).toHaveBeenCalledWith({ prompt: "go", runner: "mock" });
    expect(titles()).toEqual(["Fresh", "Session old"]);
    // The socket's own report of it must not add a second row.
    upsert("new", 500, { title: "Fresh" });
    expect(titles()).toEqual(["Fresh", "Session old"]);
  });

  it("reads health once at start", async () => {
    const { app, socket } = setup();
    expect(app.health()).toEqual({ status: "loading" });

    await settle();
    flush();
    expect(app.health()).toEqual({ status: "ready", health });

    socket.setState("open");
    flush();
    expect(app.connection()).toBe("open");
  });

  it("marks the server unreachable, and recovers when the socket opens", async () => {
    const read = vi
      .fn<Api["health"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(health);
    const { app, socket } = setup({ health: read });

    await settle();
    flush();
    expect(app.health()).toEqual({ status: "unreachable" });
    expect(app.connection()).toBe("connecting");

    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    flush();
    expect(app.health()).toEqual({ status: "ready", health });
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("keeps the last health through an outage and refreshes it after a reconnect", async () => {
    const next: Health = { ...health, dry_run: false, runners: ["claude", "mock"] };
    const read = vi
      .fn<Api["health"]>()
      .mockResolvedValueOnce(health)
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(next);
    const { app, socket } = setup({ health: read });
    await settle();
    socket.setState("open");
    await settle();
    expect(read).toHaveBeenCalledTimes(1);

    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    flush();
    expect(app.health()).toEqual({ status: "ready", health });

    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    flush();
    expect(app.health()).toEqual({ status: "ready", health: next });
  });

  it("retries on demand without waiting for the backoff", async () => {
    const read = vi
      .fn<Api["health"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(health);
    const { app, socket } = setup({ health: read });
    await settle();

    app.retry();
    await settle();
    flush();

    expect(socket.reconnects).toBe(1);
    expect(app.health()).toEqual({ status: "ready", health });
  });

  it("lets go of the socket when stopped", () => {
    const { socket } = setup();
    for (const stop of stops.splice(0)) stop();
    expect(socket.subscriberCount("sessions")).toBe(0);
  });
});
