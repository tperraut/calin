import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { EventBody } from "../gen/EventBody";
import type { RepoChanges } from "../gen/RepoChanges";
import type { Session } from "../gen/Session";
import type { SessionStatus } from "../gen/SessionStatus";
import {
  conflict,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  makeSessionRepo,
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

  it("seeds the repo set from the record and keeps it current from events", async () => {
    const seeded = [makeSessionRepo("api", { primary: true }), makeSessionRepo("web")];
    const { socket, store } = setup({
      getSession: () => Promise.resolve(makeDetail(makeSession("s1"), seeded)),
    });
    expect(store.repos()).toEqual([]);

    await settle();
    flush();
    expect(store.repos()).toEqual(seeded);

    const attach = (seq: number, repo: ReturnType<typeof makeSessionRepo>) =>
      socket.emit(TOPIC, {
        op: "event",
        topic: TOPIC,
        event: makeEvent(seq, { type: "repo_attached", repo }),
      });
    attach(1, makeSessionRepo("web", { branch: "calin/s1" }));
    attach(2, makeSessionRepo("docs", { mode: "live" }));
    flush();
    expect(store.repos()).toEqual([
      makeSessionRepo("api", { primary: true }),
      makeSessionRepo("web", { branch: "calin/s1" }),
      makeSessionRepo("docs", { mode: "live" }),
    ]);
  });

  it("has the repo set from events alone when the record lists none", () => {
    const { socket, store } = setup();
    socket.emit(TOPIC, {
      op: "event",
      topic: TOPIC,
      event: makeEvent(1, {
        type: "repo_attached",
        repo: makeSessionRepo("api", { primary: true }),
      }),
    });
    flush();
    expect(store.repos()).toEqual([makeSessionRepo("api", { primary: true })]);
  });

  it("marks the subscription failed when the server refuses the topic, until it is joined", () => {
    const { socket, store, message, subscribed } = setup();
    expect(store.subscriptionError()).toBeUndefined();

    socket.emit(TOPIC, { op: "error", topic: TOPIC, message: "unknown session" });
    flush();
    expect(store.subscriptionError()).toBe("unknown session");
    expect(store.synced()).toBe(false);

    store.resubscribe();
    expect(socket.resyncs).toEqual([{ topic: TOPIC, afterSeq: 0 }]);
    message(1);
    subscribed(1);
    flush();
    expect(store.subscriptionError()).toBeUndefined();
    expect(store.synced()).toBe(true);
  });

  it("interrupts and answers permission requests for its own session", async () => {
    const interruptSession = vi.fn<Api["interruptSession"]>().mockResolvedValue();
    const resolvePermission = vi.fn<Api["resolvePermission"]>().mockResolvedValue();
    const { store } = setup({ interruptSession, resolvePermission });

    await store.interrupt();
    await store.resolvePermission("p1", "allow");
    await store.resolvePermission("p2", "deny", "  Not on main.  ");
    await store.resolvePermission("p3", "deny", "   ");

    expect(interruptSession).toHaveBeenCalledWith("s1");
    expect(resolvePermission.mock.calls).toEqual([
      ["s1", "p1", { decision: "allow" }],
      ["s1", "p2", { decision: "deny", message: "Not on main." }],
      ["s1", "p3", { decision: "deny" }],
    ]);
  });

  it("attaches a repo to its own session without adding it: the event does that", async () => {
    const attachRepo = vi
      .fn<Api["attachRepo"]>()
      .mockResolvedValueOnce(makeSessionRepo("web"))
      .mockRejectedValue(conflict("web is already attached"));
    const { socket, store } = setup({ attachRepo });

    await expect(store.attachRepo({ repo_id: "repo-web", mode: "read" })).resolves.toBeUndefined();
    flush();
    expect(attachRepo).toHaveBeenCalledWith("s1", { repo_id: "repo-web", mode: "read" });
    // The server answered, but the repo set follows the log.
    expect(store.repos()).toEqual([]);

    socket.emit(TOPIC, {
      op: "event",
      topic: TOPIC,
      event: makeEvent(1, { type: "repo_attached", repo: makeSessionRepo("web") }),
    });
    flush();
    expect(store.repos()).toEqual([makeSessionRepo("web")]);

    await expect(store.attachRepo({ repo_id: "repo-web" })).rejects.toMatchObject({
      status: 409,
      message: "web is already attached",
    });
  });

  describe("changes", () => {
    const listing = (...paths: string[]): RepoChanges[] => [
      {
        session_repo_id: "api",
        name: "api",
        branch: null,
        base_ref: null,
        files: paths.map((path) => ({
          path,
          old_path: null,
          status: "modified",
          added: 1,
          removed: 0,
          binary: false,
        })),
        added: paths.length,
        removed: 0,
      },
    ];

    function live(getChanges: Api["getChanges"]) {
      const made = setup({ getChanges });
      let seq = 0;
      const emit = (body: EventBody) => {
        seq += 1;
        made.socket.emit(TOPIC, { op: "event", topic: TOPIC, event: makeEvent(seq, body) });
      };
      const status = (value: SessionStatus) =>
        emit({ type: "session_status", status: value, reason: null });
      const caughtUp = () => made.subscribed(seq);
      return { ...made, emit, status, caughtUp };
    }

    afterEach(() => vi.useRealTimers());

    it("reads nothing until asked, then holds the server's list and the revision it accounts for", async () => {
      const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue(listing("a.rs"));
      const { store, emit } = live(getChanges);
      await settle();
      expect(getChanges).not.toHaveBeenCalled();
      expect(store.changes.list()).toBeUndefined();

      emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
      store.changes.now();
      flush();
      expect(store.changes.refreshing()).toBe(true);
      // Named while the list was on its way: not known to be in it.
      emit({ type: "file_changed", session_repo_id: "api", path: "b.rs", op: "edit" });
      await settle();
      flush();
      expect(getChanges).toHaveBeenCalledWith("s1");
      expect(store.changes.list()).toEqual(listing("a.rs"));
      expect(store.changes.revision()).toBe(1);
      expect(store.changes.refreshing()).toBe(false);
      expect(store.view().changesRevision).toBe(2);
    });

    it("reads again when the status changes or a repo joins, live only, and debounced", async () => {
      const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue(listing());
      const { status, emit, caughtUp } = live(getChanges);
      vi.useFakeTimers();

      // The replay passes through old statuses: none of them is news.
      status("preparing");
      status("running");
      vi.advanceTimersByTime(5000);
      expect(getChanges).not.toHaveBeenCalled();

      caughtUp();
      status("waiting");
      status("running");
      vi.advanceTimersByTime(499);
      expect(getChanges).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      expect(getChanges).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(0);

      // A turn that ends: a shell may have changed files no event named.
      status("review");
      vi.advanceTimersByTime(500);
      expect(getChanges).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(0);

      emit({ type: "repo_attached", repo: makeSessionRepo("web") });
      vi.advanceTimersByTime(500);
      expect(getChanges).toHaveBeenCalledTimes(3);
      await vi.advanceTimersByTimeAsync(0);

      // A file event alone asks nothing here: whoever shows the list decides.
      emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
      emit({ type: "user_message", text: "and now?" });
      vi.advanceTimersByTime(5000);
      expect(getChanges).toHaveBeenCalledTimes(3);
    });

    it("keeps the last list when a read fails, with the server's reason, and stops with the store", async () => {
      const getChanges = vi
        .fn<Api["getChanges"]>()
        .mockResolvedValueOnce(listing("a.rs"))
        .mockRejectedValueOnce(conflict("the worktree is being prepared"))
        .mockResolvedValue(listing("a.rs", "b.rs"));
      const { store, stop } = live(getChanges);
      store.changes.now();
      await settle();
      store.changes.now();
      await settle();
      flush();
      expect(store.changes.failure()).toBe("the worktree is being prepared");
      expect(store.changes.list()).toEqual(listing("a.rs"));

      store.changes.now();
      await settle();
      flush();
      expect(store.changes.failure()).toBeUndefined();
      expect(store.changes.list()).toEqual(listing("a.rs", "b.rs"));

      stop();
      store.changes.now();
      store.changes.request();
      await settle();
      expect(getChanges).toHaveBeenCalledTimes(3);
    });

    it("reads nothing while the page is in a background tab, and catches up on the way back", async () => {
      const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue(listing());
      const { status, caughtUp } = live(getChanges);
      caughtUp();
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));

      status("review");
      vi.advanceTimersByTime(5000);
      expect(getChanges).not.toHaveBeenCalled();

      visibility.mockReturnValue("visible");
      document.dispatchEvent(new Event("visibilitychange"));
      expect(getChanges).toHaveBeenCalledTimes(1);
    });
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
