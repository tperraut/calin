import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { EventBody } from "../gen/EventBody";
import type { Health } from "../gen/Health";
import type { RepoChanges } from "../gen/RepoChanges";
import type { Session } from "../gen/Session";
import type { SessionStatus } from "../gen/SessionStatus";
import {
  conflict,
  deferred,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makePullRequest,
  makeSession,
  makeSessionRepo,
  notFound,
  settle,
  unreachable,
} from "../test/fakes";
import type { WorkflowRun } from "../gen/WorkflowRun";
import { specGraph } from "../test/workflows";
import { createSessionStore, namedRepo, SHIP_QUIET_MS } from "./sessionStore";

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

  it("ignores a failed load that answers once stopped", async () => {
    const { store, stop } = setup({ getSession: () => Promise.reject(unreachable()) });
    stop();
    await settle();
    flush();
    expect(store.load()).toEqual({ status: "loading" });
  });

  it("loads the record again on reload, saying it is loading meanwhile", async () => {
    const getSession = vi
      .fn<Api["getSession"]>()
      .mockResolvedValueOnce(makeDetail(makeSession("s1", 1000, { title: "Before" })))
      .mockResolvedValue(makeDetail(makeSession("s1", 1000, { title: "After" })));
    const { store } = setup({ getSession });
    await settle();
    flush();
    expect(store.session()?.title).toBe("Before");

    store.reload();
    flush();
    expect(store.load()).toEqual({ status: "loading" });
    await settle();
    flush();
    expect(store.load()).toEqual({ status: "ready" });
    expect(store.session()?.title).toBe("After");
    expect(getSession).toHaveBeenCalledTimes(2);
  });

  it("changes nothing for a frame that is not the session's own news", () => {
    const { socket, store, message } = setup();
    message(1);
    flush();
    const before = store.view();
    socket.emit(TOPIC, { op: "session_upserted", session: makeSession("s1") });
    flush();
    expect(store.view()).toBe(before);
    expect(store.synced()).toBe(false);
    expect(socket.resyncs).toEqual([]);
  });

  it("reports no limits when neither the agent nor the session names a runner", () => {
    const onLimits = vi.fn();
    const socket = new FakeSocket();
    const api = fakeApi({ getSession: () => new Promise(() => {}) });
    const store = createSessionStore({ api, socket, schedule: immediately, onLimits }, "s1");
    store.start();
    socket.emit(TOPIC, {
      op: "event",
      topic: TOPIC,
      event: makeEvent(1, { type: "limits", windows: [] }, "ghost"),
    });
    expect(onLimits).not.toHaveBeenCalled();
    flush();
    expect(store.view().seq).toBe(1);
  });

  it("is alone in its tree until the sessions list is given", async () => {
    const { store } = setup();
    expect(store.tree().sessions).toEqual([]);
    await settle();
    flush();
    expect(store.tree().sessions.map((one) => one.id)).toEqual(["s1"]);
  });

  it("reads and writes its repos through its own session's routes", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue([]);
    const getDiff = vi.fn<Api["getDiff"]>().mockRejectedValue(notFound());
    const listTree = vi.fn<Api["listTree"]>().mockResolvedValue([]);
    const readFile = vi.fn<Api["readFile"]>().mockRejectedValue(notFound());
    const writeFile = vi.fn<Api["writeFile"]>().mockRejectedValue(conflict("changed on disk"));
    const { store } = setup({ getChanges, getDiff, listTree, readFile, writeFile });
    const body = { text: "x", expected_hash: "h1" };

    await expect(store.workspace.changes()).resolves.toEqual([]);
    await expect(store.workspace.diff("api", "a.rs")).rejects.toMatchObject({ status: 404 });
    await expect(store.workspace.tree("api", "")).resolves.toEqual([]);
    await expect(store.workspace.readFile("api", "a.rs")).rejects.toMatchObject({ status: 404 });
    await expect(store.workspace.writeFile("api", "a.rs", body)).rejects.toThrow("changed on disk");
    expect(getChanges).toHaveBeenCalledWith("s1");
    expect(getDiff).toHaveBeenCalledWith("s1", "api", "a.rs");
    expect(listTree).toHaveBeenCalledWith("s1", "api", "");
    expect(readFile).toHaveBeenCalledWith("s1", "api", "a.rs");
    expect(writeFile).toHaveBeenCalledWith("s1", "api", "a.rs", body);
  });

  it("says a terminal cannot be opened where none can, after the caller has its client", async () => {
    const { store } = setup();
    const onEnd = vi.fn();
    const client = store.workspace.openTerminal({ cols: 80, rows: 24 }, { onData() {}, onEnd });
    expect(onEnd).not.toHaveBeenCalled();
    client.write("ls\n");
    await settle();
    expect(onEnd.mock.calls).toEqual([
      [{ kind: "error", message: "Terminals are not available here." }],
    ]);
  });
});

describe("namedRepo", () => {
  const covered = new Map([
    ["r1", "web"],
    ["r2", "web-admin"],
    ["r3", "api.v2"],
  ]);

  it.each([
    ["web: the push was rejected", "r1"],
    ["could not push web-admin: protected branch", "r2"],
    ["api.v2 has no remote", "r3"],
    // A name inside a longer word is not that repo.
    ["webhook failed", undefined],
    ["apixv2 has no remote", undefined],
    ["GitHub answered 502", undefined],
  ])("reads %j as naming %s", (message, id) => {
    expect(namedRepo(covered, message)).toBe(id);
  });
});

describe("session store: delivery", () => {
  const repos = [
    makeSessionRepo("api", { primary: true }),
    makeSessionRepo("web"),
    makeSessionRepo("docs", { mode: "read" }),
  ];
  const changed = (id: string, files = 1): RepoChanges => ({
    session_repo_id: id,
    name: id,
    branch: null,
    base_ref: null,
    files: Array.from({ length: files }, (_, index) => ({
      path: `f${index}.rs`,
      old_path: null,
      status: "modified",
      added: 1,
      removed: 0,
      binary: false,
    })),
    added: files,
    removed: 0,
  });
  const health: Health = {
    name: "calin",
    version: "0.1.0",
    dry_run: false,
    runners: ["mock"],
    github_problem: null,
  };

  /** A session in review whose `api` and `web` worktrees changed, with the changes list read. */
  async function inReview(
    routes: Partial<Api> = {},
    options: {
      seed?: ReturnType<typeof makePullRequest>[];
      health?: () => Health | undefined;
    } = {},
  ) {
    const socket = new FakeSocket();
    const checkHealth = vi.fn();
    const api = fakeApi({
      getSession: () =>
        Promise.resolve(
          makeDetail(makeSession("s1", 1000, { status: "review" }), repos, options.seed ?? []),
        ),
      getChanges: () => Promise.resolve([changed("api", 3), changed("web"), changed("docs")]),
      shipSession: () => Promise.resolve(),
      ...routes,
    });
    const store = createSessionStore(
      { api, socket, schedule: immediately, health: options.health, checkHealth, now: () => 5000 },
      "s1",
    );
    const stop = store.start();
    let seq = 0;
    const emit = (body: EventBody) => {
      seq += 1;
      socket.emit(TOPIC, { op: "event", topic: TOPIC, event: makeEvent(seq, body) });
      flush();
    };
    const caughtUp = () => {
      socket.emit(TOPIC, { op: "subscribed", topic: TOPIC, head_seq: seq });
      flush();
    };
    await settle();
    store.changes.now();
    await settle();
    flush();
    return { store, socket, stop, emit, caughtUp, checkHealth };
  }
  const opened = (repo: string, number: number, patch = {}): EventBody => ({
    type: "pull_request_opened",
    pull_request: makePullRequest(repo, number, patch),
  });

  afterEach(() => vi.useRealTimers());

  it("starts from the record's pull requests and follows the events, in repo-set order", async () => {
    const { store, emit, caughtUp } = await inReview(
      {},
      { seed: [makePullRequest("web", 7, { updated_at: 900 })] },
    );
    expect(store.delivery.pullRequests()).toEqual([makePullRequest("web", 7, { updated_at: 900 })]);

    // The replay passes through the pull request's past: the record's newer value stays meanwhile.
    emit(opened("web", 7, { state: "draft", updated_at: 100 }));
    expect(store.delivery.pullRequests().map((one) => one.state)).toEqual(["open"]);
    emit(opened("api", 12, { updated_at: 950 }));
    caughtUp();
    expect(store.delivery.pullRequests().map((one) => one.session_repo_id)).toEqual(["api", "web"]);
    expect(store.delivery.pullRequests()[1].state).toBe("draft");

    emit({
      type: "pull_request_updated",
      pull_request: makePullRequest("web", 7, { state: "merged", updated_at: 2000 }),
    });
    expect(store.delivery.pullRequests().map((one) => [one.session_repo_id, one.state])).toEqual([
      ["api", "open"],
      ["web", "merged"],
    ]);
  });

  it("sends the request and stays `opening` until every repo it covers has a pull request", async () => {
    const shipSession = vi.fn<Api["shipSession"]>().mockResolvedValue(undefined);
    const { store, emit, caughtUp } = await inReview({ shipSession });
    caughtUp();
    expect(store.delivery.shipping()).toBeUndefined();

    await store.delivery.ship({ title: "Add a health check", draft: true });
    flush();
    expect(shipSession).toHaveBeenCalledWith("s1", { title: "Add a health check", draft: true });
    expect(store.delivery.shipping()).toBe("opening");
    // Nothing is assumed: no pull request shows before its event.
    expect(store.delivery.pullRequests()).toEqual([]);

    emit(opened("api", 12));
    emit({ type: "session_status", status: "shipping", reason: null });
    expect(store.delivery.shipping()).toBe("opening");
    emit(opened("web", 7));
    expect(store.delivery.shipping()).toBeUndefined();
    expect(store.delivery.failed()).toBe(0);
  });

  it("does not take a replayed pull request for the outcome of this ship", async () => {
    const { store, emit, caughtUp } = await inReview();
    emit(opened("api", 12));
    void store.delivery.ship({});
    emit(opened("web", 7));
    caughtUp();
    await settle();
    flush();
    // Both were in the log already: the ship asked just now has reported nothing yet.
    expect(store.delivery.shipping()).toBe("updating");
  });

  it("counts the repos that failed, by the name their error carries, and lets the others carry on", async () => {
    const { store, emit, caughtUp } = await inReview();
    caughtUp();
    await store.delivery.ship({});
    emit({ type: "error", message: "web: the push was rejected (protected branch)" });
    expect(store.delivery.failed()).toBe(1);
    expect(store.delivery.shipping()).toBe("opening");
    emit(opened("api", 12));
    expect(store.delivery.shipping()).toBeUndefined();
    expect(store.delivery.failed()).toBe(1);

    // The next ship starts the count again.
    await store.delivery.ship({});
    flush();
    expect(store.delivery.failed()).toBe(0);
    expect(store.delivery.shipping()).toBe("updating");
  });

  it("counts an error that names no repo as one repo fewer to wait for", async () => {
    const { store, emit, caughtUp } = await inReview();
    caughtUp();
    await store.delivery.ship({});
    emit({ type: "error", message: "GitHub answered 502" });
    emit({ type: "error", message: "GitHub answered 502" });
    expect(store.delivery.shipping()).toBeUndefined();
    expect(store.delivery.failed()).toBe(2);
  });

  it("rejects with the server's reason, stops saying it ships, and can be asked again", async () => {
    const shipSession = vi
      .fn<Api["shipSession"]>()
      .mockRejectedValueOnce(conflict("the gh CLI is not signed in"))
      .mockResolvedValue(undefined);
    const { store, caughtUp } = await inReview({ shipSession });
    caughtUp();
    await expect(store.delivery.ship({})).rejects.toMatchObject({
      status: 409,
      message: "the gh CLI is not signed in",
    });
    flush();
    expect(store.delivery.shipping()).toBeUndefined();
    await store.delivery.ship({});
    flush();
    expect(store.delivery.shipping()).toBe("opening");
  });

  it("sends one request at a time, and sees events that beat the answer", async () => {
    const answer = deferred();
    const shipSession = vi.fn<Api["shipSession"]>().mockReturnValue(answer.promise);
    const { store, emit, caughtUp } = await inReview({ shipSession });
    caughtUp();
    const first = store.delivery.ship({});
    flush();
    expect(store.delivery.shipping()).toBe("opening");
    await store.delivery.ship({});
    expect(shipSession).toHaveBeenCalledTimes(1);

    emit(opened("api", 12));
    emit(opened("web", 7));
    expect(store.delivery.shipping()).toBeUndefined();
    answer.resolve();
    await first;
    flush();
    expect(store.delivery.shipping()).toBeUndefined();
  });

  it("stops waiting when the session goes elsewhere, or when the ship stays silent", async () => {
    vi.useFakeTimers();
    const { store, emit, caughtUp } = await inReview();
    caughtUp();
    await store.delivery.ship({});
    emit({ type: "session_status", status: "running", reason: null });
    expect(store.delivery.shipping()).toBeUndefined();

    emit({ type: "session_status", status: "review", reason: null });
    await store.delivery.ship({});
    flush();
    expect(store.delivery.shipping()).toBe("opening");
    emit(opened("api", 12));
    vi.advanceTimersByTime(SHIP_QUIET_MS - 1);
    flush();
    expect(store.delivery.shipping()).toBe("opening");
    vi.advanceTimersByTime(1);
    flush();
    expect(store.delivery.shipping()).toBeUndefined();
  });

  it("does not wait longer for events that are not the ship's news", async () => {
    vi.useFakeTimers();
    const { store, emit, caughtUp } = await inReview();
    caughtUp();
    await store.delivery.ship({});
    vi.advanceTimersByTime(SHIP_QUIET_MS / 2);
    emit({ type: "user_message", text: "meanwhile" });
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    vi.advanceTimersByTime(SHIP_QUIET_MS / 2 - 1);
    flush();
    expect(store.delivery.shipping()).toBe("opening");
    expect(store.delivery.failed()).toBe(0);
    vi.advanceTimersByTime(1);
    flush();
    expect(store.delivery.shipping()).toBeUndefined();
  });

  it("covers no repo when asked before the record has loaded, and stops once answered", async () => {
    const answer = deferred();
    const shipSession = vi.fn<Api["shipSession"]>().mockReturnValue(answer.promise);
    const socket = new FakeSocket();
    const api = fakeApi({ getSession: () => new Promise(() => {}), shipSession });
    const store = createSessionStore({ api, socket, schedule: immediately }, "s1");
    store.start();
    socket.emit(TOPIC, { op: "subscribed", topic: TOPIC, head_seq: 0 });
    const shipped = store.delivery.ship({});
    flush();
    expect(store.delivery.shipping()).toBe("opening");
    answer.resolve();
    await shipped;
    flush();
    expect(shipSession).toHaveBeenCalledWith("s1", {});
    expect(store.delivery.shipping()).toBeUndefined();
  });

  it("does not let a late refusal of an earlier ship end the one under way", async () => {
    const first = deferred();
    const second = deferred();
    const shipSession = vi
      .fn<Api["shipSession"]>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const { store, emit, caughtUp } = await inReview({ shipSession });
    caughtUp();
    const earlier = store.delivery.ship({});
    emit(opened("api", 12));
    emit(opened("web", 7));
    expect(store.delivery.shipping()).toBeUndefined();

    void store.delivery.ship({});
    flush();
    expect(store.delivery.shipping()).toBe("updating");
    first.reject(conflict("too late"));
    await expect(earlier).rejects.toThrow("too late");
    flush();
    expect(store.delivery.shipping()).toBe("updating");
    expect(shipSession).toHaveBeenCalledTimes(2);
  });

  it("keeps one still clock when the app gives none", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(123_000);
    const store = createSessionStore({ api: fakeApi(), socket: new FakeSocket() }, "s1");
    vi.setSystemTime(456_000);
    expect(store.delivery.now()).toBe(123_000);
    expect(store.delivery.blocked()).toBeUndefined();
    expect(store.delivery.dryRun()).toBe(false);
    store.delivery.recheck();
  });

  it("says why pull requests cannot be opened, unless the server only pretends to open them", async () => {
    let current: Health | undefined;
    const { store, checkHealth } = await inReview({}, { health: () => current });
    expect(store.delivery.blocked()).toBeUndefined();
    expect(store.delivery.dryRun()).toBe(false);

    current = { ...health, github_problem: "the gh CLI is not signed in" };
    expect(store.delivery.blocked()).toBe("the gh CLI is not signed in");
    // api.rs, "Dry run": GitHub is never called, so it cannot be in the way.
    current = { ...health, dry_run: true, github_problem: "the gh CLI is missing" };
    expect(store.delivery.blocked()).toBeUndefined();
    expect(store.delivery.dryRun()).toBe(true);

    store.delivery.recheck();
    expect(checkHealth).toHaveBeenCalledTimes(1);
    expect(store.delivery.now()).toBe(5000);
  });

  it("asks the server to look at the pull requests, and passes its refusal on", async () => {
    const refreshPullRequests = vi
      .fn<Api["refreshPullRequests"]>()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(conflict("GitHub cannot be reached"));
    const { store } = await inReview({ refreshPullRequests });
    await expect(store.delivery.refresh()).resolves.toBeUndefined();
    expect(refreshPullRequests).toHaveBeenCalledWith("s1");
    await expect(store.delivery.refresh()).rejects.toThrow("GitHub cannot be reached");
  });
});

describe("session store: the workflow run", () => {
  const served = (cost: number): WorkflowRun => ({
    workflow_id: "itr",
    name: "Implement",
    graph: specGraph(),
    nodes: specGraph().nodes.map((one) => ({
      id: one.id,
      state: "idle",
      runs: 0,
      last_port: null,
      outcome: null,
      iteration: one.kind === "loop" ? 0 : null,
      max: one.kind === "loop" ? 3 : null,
    })),
    taken: [],
    status: "running",
    reason: null,
    budget_usd: 5,
    cost_usd: cost,
  });
  const withWorkflow = () =>
    Promise.resolve(makeDetail(makeSession("s1", 1000, { status: "running", workflow: "itr" })));

  function live(getWorkflow: Api["getWorkflow"], getSession: Api["getSession"] = withWorkflow) {
    const made = setup({ getWorkflow, getSession });
    let seq = 0;
    const emit = (body: EventBody) => {
      seq += 1;
      made.socket.emit(TOPIC, { op: "event", topic: TOPIC, event: makeEvent(seq, body) });
    };
    const caughtUp = () => made.subscribed(seq);
    return { ...made, emit, caughtUp };
  }

  const begin: EventBody = {
    type: "workflow_started",
    workflow_id: "itr",
    name: "Implement",
    graph: specGraph(),
    budget_usd: 5,
  };
  const node = (node_id: string, phase: "started" | "finished", port: string | null = null) =>
    ({ type: "workflow_node", node_id, phase, port, outcome: null }) as const;

  afterEach(() => vi.useRealTimers());

  it("reads the run once the record says there is one, and adds loop limits and cost to the events", async () => {
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockResolvedValue(served(1.25));
    const { store, emit, caughtUp } = live(getWorkflow);
    expect(store.workflow.known()).toBe(false);
    await settle();
    flush();
    expect(getWorkflow).toHaveBeenCalledTimes(1);
    expect(getWorkflow).toHaveBeenCalledWith("s1");
    expect(store.workflow.known()).toBe(true);

    emit(begin);
    emit(node("retry", "started"));
    emit(node("retry", "finished", "repeat"));
    emit(node("retry", "started"));
    caughtUp();
    await settle();
    flush();
    // Seeded once: catching up does not read it again.
    expect(getWorkflow).toHaveBeenCalledTimes(1);
    const run = store.workflow.run();
    expect(run?.cost_usd).toBe(1.25);
    expect(run?.nodes.find((one) => one.id === "retry")).toMatchObject({
      state: "active",
      runs: 2,
      iteration: 2,
      max: 3,
    });
  });

  it("takes the server's 404 as no run", async () => {
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockResolvedValue(null);
    const { store } = live(getWorkflow);
    await settle();
    flush();
    expect(getWorkflow).toHaveBeenCalledTimes(1);
    expect(store.workflow.run()).toBeUndefined();
    expect(store.workflow.failure()).toBeUndefined();
  });

  it("reads nothing for a session without a workflow", async () => {
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockResolvedValue(null);
    const { store, caughtUp } = live(getWorkflow, () =>
      Promise.resolve(makeDetail(makeSession("s1", 1000, { status: "running" }))),
    );
    caughtUp();
    await settle();
    flush();
    expect(getWorkflow).not.toHaveBeenCalled();
    expect(store.workflow.known()).toBe(false);
  });

  it("reads again, debounced, when a node moves or usage arrives while the view is on screen", async () => {
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockResolvedValue(served(0.5));
    const { store, emit, caughtUp } = live(getWorkflow);
    emit(begin);
    caughtUp();
    await settle();
    vi.useFakeTimers();
    const seeded = getWorkflow.mock.calls.length;

    // Not on screen: nothing is read for events.
    emit(node("implement", "started"));
    vi.advanceTimersByTime(5000);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded);

    // Opening the view reads at once (it was stale).
    store.workflow.setVisible(true);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded + 1);
    await vi.advanceTimersByTimeAsync(0);

    emit(node("implement", "finished", "done"));
    emit({ type: "usage", cost_usd: 0.7, tokens_in: 10, tokens_out: 5 });
    vi.advanceTimersByTime(499);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded + 1);
    vi.advanceTimersByTime(1);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded + 2);
    await vi.advanceTimersByTimeAsync(0);

    // Other events are not the run's news.
    emit({ type: "user_message", text: "faster" });
    vi.advanceTimersByTime(5000);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded + 2);

    store.workflow.setVisible(false);
    emit(node("tests", "started"));
    vi.advanceTimersByTime(5000);
    expect(getWorkflow).toHaveBeenCalledTimes(seeded + 2);
  });

  it.each([
    [
      "a run",
      (answer: ReturnType<typeof deferred<WorkflowRun | null>>) => answer.resolve(served(2)),
    ],
    [
      "a refusal",
      (answer: ReturnType<typeof deferred<WorkflowRun | null>>) =>
        answer.reject(conflict("store busy")),
    ],
  ])("ignores %s that answers once the store is stopped", async (_name, settleWith) => {
    const answer = deferred<WorkflowRun | null>();
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockReturnValue(answer.promise);
    const { store, stop } = live(getWorkflow);
    await settle();
    expect(getWorkflow).toHaveBeenCalledTimes(1);
    stop();
    settleWith(answer);
    await settle();
    flush();
    expect(store.workflow.run()).toBeUndefined();
    expect(store.workflow.failure()).toBeUndefined();
  });

  it("keeps the server's reason when a read fails", async () => {
    const getWorkflow = vi.fn<Api["getWorkflow"]>().mockRejectedValue(conflict("store busy"));
    const { store } = live(getWorkflow);
    await settle();
    flush();
    expect(store.workflow.failure()).toBe("store busy");
  });

  it("answers a gate with its comment only when there is one", async () => {
    const answerQuestion = vi.fn<Api["answerQuestion"]>().mockResolvedValue();
    const { store } = setup({ answerQuestion });
    await store.answerQuestion("g1", "approve");
    await store.answerQuestion("g1", " reject ", "  Not yet.  ");
    await store.answerQuestion("g1", "approve", "   ");
    expect(answerQuestion.mock.calls).toEqual([
      ["s1", "g1", { answer: "approve" }],
      ["s1", "g1", { answer: "reject", comment: "Not yet." }],
      ["s1", "g1", { answer: "approve" }],
    ]);
  });
});
