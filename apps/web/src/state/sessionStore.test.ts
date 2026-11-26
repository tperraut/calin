import { flush } from "solid-js";
import { describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Session } from "../gen/Session";
import {
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  notFound,
  settle,
  unreachable,
} from "../test/fakes";
import { createSessionStore } from "./sessionStore";

const TOPIC = "session:s1";

function setup(routes: Partial<Api> = {}, listed?: () => Session | undefined) {
  const socket = new FakeSocket();
  const api = fakeApi({
    getSession: () => Promise.resolve(makeDetail(makeSession("s1", 1000, { status: "queued" }))),
    ...routes,
  });
  const store = createSessionStore({ api, socket, schedule: immediately, listed }, "s1");
  const stop = store.start();
  const message = (seq: number) =>
    socket.emit(TOPIC, {
      op: "event",
      topic: TOPIC,
      event: makeEvent(seq, { type: "user_message", text: `m${seq}` }),
    });
  const subscribed = (headSeq: number) =>
    socket.emit(TOPIC, { op: "subscribed", topic: TOPIC, head_seq: headSeq });
  const texts = () => {
    flush();
    return store
      .view()
      .transcript.map((entry) => (entry.kind === "user" ? entry.text : entry.kind));
  };
  return { socket, store, stop, message, subscribed, texts };
}

describe("session store", () => {
  it("loads the session record and folds the replay into the view", async () => {
    const { store, message, subscribed, texts } = setup();
    expect(store.load()).toEqual({ status: "loading" });

    await settle();
    flush();
    expect(store.load()).toEqual({ status: "ready" });
    expect(store.session()?.title).toBe("Session s1");

    message(1);
    message(2);
    expect(texts()).toEqual(["m1", "m2"]);
    expect(store.synced()).toBe(false);

    subscribed(2);
    flush();
    expect(store.synced()).toBe(true);
    expect(store.view().seq).toBe(2);
  });

  it("shows the recorded status until the replay has caught up", async () => {
    const { socket, store, subscribed } = setup();
    await settle();
    socket.emit(TOPIC, {
      op: "event",
      topic: TOPIC,
      event: makeEvent(1, { type: "session_status", status: "running", reason: null }),
    });
    flush();
    expect(store.status()).toBe("queued");

    subscribed(1);
    flush();
    expect(store.status()).toBe("running");
  });

  it("prefers the session the sessions topic reported over the one it fetched", async () => {
    const live = makeSession("s1", 1000, { title: "Live title", status: "review" });
    const { store } = setup({}, () => live);
    await settle();
    flush();
    expect(store.session()).toBe(live);
    expect(store.status()).toBe("review");
  });

  it("resumes from the last applied seq", () => {
    const { socket, message } = setup();
    expect(socket.cursor(TOPIC)).toBe(0);
    message(1);
    message(2);
    message(2);
    expect(socket.cursor(TOPIC)).toBe(2);
  });

  it("resubscribes from the last applied seq when an event is missing", () => {
    const { socket, message, subscribed, texts } = setup();
    message(1);
    subscribed(1);
    message(2);

    message(4);
    message(5);
    // One request covers the gap; frames that were already in flight do not add more.
    expect(socket.resyncs).toEqual([{ topic: TOPIC, afterSeq: 2 }]);
    expect(texts()).toEqual(["m1", "m2"]);

    for (const seq of [3, 4, 5]) message(seq);
    subscribed(5);
    expect(texts()).toEqual(["m1", "m2", "m3", "m4", "m5"]);
    expect(socket.resyncs).toHaveLength(1);
  });

  it("does not treat frames from before the first replay as a gap", () => {
    const { socket, message, subscribed, texts } = setup();
    // Left over from an earlier subscription to the same topic.
    message(7);
    expect(socket.resyncs).toEqual([]);

    for (const seq of [1, 2]) message(seq);
    subscribed(2);
    expect(texts()).toEqual(["m1", "m2"]);
    expect(socket.resyncs).toEqual([]);
  });

  it("asks again when the replay ends short of the announced head", () => {
    const { socket, store, message, subscribed } = setup();
    message(1);
    subscribed(3);
    flush();
    expect(socket.resyncs).toEqual([{ topic: TOPIC, afterSeq: 1 }]);
    expect(store.synced()).toBe(false);
  });

  it("stops asking and says so when the same gap keeps coming back", () => {
    const { socket, store, message, subscribed } = setup();
    message(1);
    for (let round = 0; round < 10; round += 1) subscribed(3);
    flush();

    expect(socket.resyncs).toHaveLength(3);
    expect(store.stalled()).toBe(true);
    // What did arrive is shown rather than an endless loading state.
    expect(store.synced()).toBe(true);

    message(2);
    flush();
    expect(store.stalled()).toBe(false);
  });

  it("reports an unknown session and stops listening", async () => {
    const { socket, store } = setup({ getSession: () => Promise.reject(notFound()) });
    expect(socket.subscriberCount(TOPIC)).toBe(1);

    await settle();
    flush();
    expect(store.load()).toEqual({ status: "not_found" });
    expect(socket.subscriberCount(TOPIC)).toBe(0);
  });

  it("reports an unreachable server, then loads once the socket is open again", async () => {
    const getSession = vi
      .fn<Api["getSession"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(makeDetail(makeSession("s1")));
    const { socket, store } = setup({ getSession });

    await settle();
    flush();
    expect(store.load()).toEqual({
      status: "error",
      message: "Cannot reach the server",
      unreachable: true,
    });

    socket.setState("open");
    await settle();
    flush();
    expect(store.load()).toEqual({ status: "ready" });
    expect(getSession).toHaveBeenCalledTimes(2);

    // A healthy session is not fetched again on every reconnect.
    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    expect(getSession).toHaveBeenCalledTimes(2);
  });

  it("sends messages and stop requests for its own session", async () => {
    const sendMessage = vi.fn<Api["sendMessage"]>().mockResolvedValue();
    const stopSession = vi.fn<Api["stopSession"]>().mockResolvedValue();
    const { store } = setup({ sendMessage, stopSession });

    await store.send("carry on");
    await store.stop();

    expect(sendMessage).toHaveBeenCalledWith("s1", { text: "carry on" });
    expect(stopSession).toHaveBeenCalledWith("s1");
  });

  it("unsubscribes and ignores late answers once stopped", async () => {
    const { socket, store, stop } = setup();
    stop();
    expect(socket.subscriberCount(TOPIC)).toBe(0);

    await settle();
    flush();
    expect(store.load()).toEqual({ status: "loading" });
  });
});
