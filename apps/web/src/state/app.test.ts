import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { HttpError, type Api } from "../api/http";
import type { Health } from "../gen/Health";
import type { Me } from "../gen/Me";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { Settings } from "../gen/Settings";
import {
  badRequest,
  conflict,
  deferred,
  fakeApi,
  FakeDevice,
  FakeSocket,
  immediately,
  makeDetail,
  makeDeviceSubscription,
  makeMissingRepo,
  makeRepo,
  makeSession,
  settle,
  signedOut,
  unreachable,
} from "../test/fakes";
import { makeIssue } from "../test/integrations";
import { workflowInfo } from "../test/workflows";
import {
  authPending,
  createAppState,
  meAuth,
  meFailure,
  meRetryDelay,
  type AppServices,
} from "./app";

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: true,
  runners: ["mock"],
  github_problem: null,
};

const stops: Array<() => void> = [];

function setup(routes: Partial<Api> = {}, services: Partial<AppServices> = {}) {
  const socket = new FakeSocket();
  const api = fakeApi({ health: () => Promise.resolve(health), ...routes });
  const app = createAppState({ api, socket, schedule: immediately, ...services });
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
  it("lists sessions from the sessions topic, newest first", async () => {
    const { socket, app, upsert, titles } = setup();
    await settle();
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

  it("updates a listed session in place", async () => {
    const { app, upsert, titles } = setup();
    await settle();
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
    await settle();
    upsert("old", 100);

    const request = { prompt: "go", runner: "mock", repos: [] };
    await expect(app.createSession(request)).resolves.toBe(created);

    expect(createSession).toHaveBeenCalledWith(request);
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

  it("says why pull requests cannot be opened once the server does, and follows a reconnect", async () => {
    const read = vi
      .fn<Api["health"]>()
      .mockResolvedValueOnce({ ...health, github_problem: "the gh CLI is not signed in" })
      .mockResolvedValue(health);
    const { app, socket } = setup({ health: read });
    // Not known yet: nothing is claimed.
    expect(app.githubProblem()).toBeUndefined();
    await settle();
    flush();
    expect(app.githubProblem()).toBe("the gh CLI is not signed in");

    socket.setState("open");
    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    flush();
    expect(app.githubProblem()).toBeUndefined();
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

  it("reads the repo registry on demand, the home repo first and the others by name", async () => {
    const home = makeRepo("notes", { home: true });
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockResolvedValue([makeRepo("web"), home, makeRepo("api")]);
    const { app } = setup({ listRepos });
    expect(app.repos()).toEqual({ status: "loading" });
    await settle();
    expect(listRepos).not.toHaveBeenCalled();

    app.loadRepos();
    await settle();
    flush();
    expect(app.repos()).toEqual({
      status: "ready",
      repos: [home, makeRepo("api"), makeRepo("web")],
    });
  });

  it("reads the home repo's sets with the repos, and does without them when they fail", async () => {
    const sets: RepoSet[] = [{ name: "storefront", repo_ids: ["api", "web"] }];
    const listRepoSets = vi
      .fn<Api["listRepoSets"]>()
      .mockResolvedValueOnce(sets)
      .mockRejectedValue(unreachable());
    const { app } = setup({ listRepos: () => Promise.resolve([makeRepo("api")]), listRepoSets });
    expect(app.repoSets()).toEqual([]);

    app.loadRepos();
    await settle();
    flush();
    expect(app.repoSets()).toEqual(sets);

    // A later read that fails leaves the last known sets, and the repos, in place.
    app.loadRepos();
    await settle();
    flush();
    expect(app.repoSets()).toEqual(sets);
    expect(app.repos().status).toBe("ready");
  });

  describe("cloning", () => {
    beforeEach(() => vi.useFakeTimers());
    afterEach(() => vi.useRealTimers());

    /** Lets what is already resolved land, without moving the clock. */
    const landed = async (): Promise<void> => {
      await vi.advanceTimersByTimeAsync(0);
      flush();
    };
    const repoOf = (app: ReturnType<typeof setup>["app"], id: string) => {
      const registry = app.repos();
      return registry.status === "ready" ? registry.repos.find((one) => one.id === id) : undefined;
    };

    it("asks for the clone, shows it at once, then reads the list every two seconds until it ends", async () => {
      const missing = makeMissingRepo("web");
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([missing]);
      const cloneRepo = vi.fn<Api["cloneRepo"]>().mockResolvedValue();
      const { app } = setup({ listRepos, cloneRepo });
      app.loadRepos();
      await landed();
      expect(listRepos).toHaveBeenCalledTimes(1);

      // Nothing is cloning: nothing is polled.
      await vi.advanceTimersByTimeAsync(10_000);
      expect(listRepos).toHaveBeenCalledTimes(1);

      listRepos.mockResolvedValue([{ ...missing, cloning: true }]);
      await app.cloneRepo("web");
      flush();
      expect(cloneRepo).toHaveBeenCalledWith("web");
      // 202 means it runs: the row says so before the next read.
      expect(repoOf(app, "web")).toMatchObject({ cloning: true, available: false });
      expect(listRepos).toHaveBeenCalledTimes(1);

      await vi.advanceTimersByTimeAsync(1999);
      expect(listRepos).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(1);
      expect(listRepos).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(2000);
      expect(listRepos).toHaveBeenCalledTimes(3);

      listRepos.mockResolvedValue([makeRepo("web", { declared: true, url: missing.url })]);
      await vi.advanceTimersByTimeAsync(2000);
      flush();
      expect(listRepos).toHaveBeenCalledTimes(4);
      expect(repoOf(app, "web")).toMatchObject({ cloning: false, available: true });

      // Done: the polling stops.
      await vi.advanceTimersByTimeAsync(20_000);
      expect(listRepos).toHaveBeenCalledTimes(4);
    });

    it("stops polling when the clone fails, and keeps the reason the server gives", async () => {
      const missing = makeMissingRepo("web");
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([missing]);
      const { app } = setup({ listRepos, cloneRepo: () => Promise.resolve() });
      app.loadRepos();
      await landed();

      await app.cloneRepo("web");
      listRepos.mockResolvedValue([{ ...missing, problem: "git clone failed: permission denied" }]);
      await vi.advanceTimersByTimeAsync(2000);
      flush();
      expect(repoOf(app, "web")).toMatchObject({
        cloning: false,
        available: false,
        problem: "git clone failed: permission denied",
      });
      const reads = listRepos.mock.calls.length;
      await vi.advanceTimersByTimeAsync(20_000);
      expect(listRepos).toHaveBeenCalledTimes(reads);
    });

    it("passes on a refused clone and polls nothing", async () => {
      const refusal = conflict("web has no url to clone from");
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([makeMissingRepo("web")]);
      const { app } = setup({ listRepos, cloneRepo: () => Promise.reject(refusal) });
      app.loadRepos();
      await landed();

      await expect(app.cloneRepo("web")).rejects.toBe(refusal);
      flush();
      expect(repoOf(app, "web")).toMatchObject({ cloning: false });
      await vi.advanceTimersByTimeAsync(20_000);
      expect(listRepos).toHaveBeenCalledTimes(1);
    });

    it("keeps polling through a read that fails, and stops with the app", async () => {
      const cloning = makeMissingRepo("web", { cloning: true });
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([cloning]);
      const { app } = setup({ listRepos });
      app.loadRepos();
      await landed();

      listRepos.mockRejectedValueOnce(unreachable());
      await vi.advanceTimersByTimeAsync(2000);
      expect(listRepos).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(2000);
      expect(listRepos).toHaveBeenCalledTimes(3);

      for (const stop of stops.splice(0)) stop();
      await vi.advanceTimersByTimeAsync(20_000);
      expect(listRepos).toHaveBeenCalledTimes(3);
    });

    it("drops a read that was on its way when the clone was accepted", async () => {
      const missing = makeMissingRepo("web");
      const early = deferred<Repo[]>();
      const listRepos = vi
        .fn<Api["listRepos"]>()
        .mockResolvedValueOnce([missing])
        .mockReturnValueOnce(early.promise)
        .mockResolvedValue([{ ...missing, cloning: true }]);
      const { app } = setup({ listRepos, cloneRepo: () => Promise.resolve() });
      app.loadRepos();
      await landed();

      app.loadRepos();
      await app.cloneRepo("web");
      // The answer from before the clone says "not cloning": it must not undo what is on screen.
      early.resolve([missing]);
      await landed();
      expect(repoOf(app, "web")).toMatchObject({ cloning: true });
      expect(listRepos).toHaveBeenCalledTimes(3);
    });
  });

  it("rescans: the server's answer becomes the list, and the sets are read again", async () => {
    const listRepoSets = vi.fn<Api["listRepoSets"]>().mockResolvedValue([]);
    const refreshRepos = vi
      .fn<Api["refreshRepos"]>()
      .mockResolvedValue([makeRepo("web"), makeRepo("api")]);
    const { app } = setup({
      listRepos: () => Promise.resolve([makeRepo("api")]),
      listRepoSets,
      refreshRepos,
    });
    app.loadRepos();
    await settle();
    expect(listRepoSets).toHaveBeenCalledTimes(1);

    await app.rescanRepos();
    await settle();
    flush();
    expect(refreshRepos).toHaveBeenCalledTimes(1);
    expect(app.repos()).toEqual({ status: "ready", repos: [makeRepo("api"), makeRepo("web")] });
    expect(listRepoSets).toHaveBeenCalledTimes(2);
  });

  it("reads the settings on demand, and reports a failure until they can be read", async () => {
    const stored: Settings = {
      home_repo: null,
      scan_roots: [],
      clone_root: null,
      max_parallel: null,
    };
    const getSettings = vi
      .fn<Api["getSettings"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(stored);
    const { app, socket } = setup({ getSettings });
    expect(app.settings()).toEqual({ status: "loading" });
    await settle();
    expect(getSettings).not.toHaveBeenCalled();

    app.loadSettings();
    await settle();
    flush();
    expect(app.settings()).toEqual({ status: "error", message: "Cannot reach the server" });

    socket.setState("open");
    await settle();
    flush();
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
  });

  it("saves the settings as the server stored them, and reads the repos again", async () => {
    const sent: Settings = {
      home_repo: " /code/home",
      scan_roots: [],
      clone_root: null,
      max_parallel: null,
    };
    const stored: Settings = {
      home_repo: "/code/home",
      scan_roots: [],
      clone_root: "/data/repos",
      max_parallel: null,
    };
    const putSettings = vi.fn<Api["putSettings"]>().mockResolvedValue(stored);
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockResolvedValueOnce([])
      .mockResolvedValue([makeRepo("home", { home: true })]);
    const { app } = setup({ putSettings, listRepos });
    app.loadRepos();
    await settle();

    await expect(app.saveSettings(sent)).resolves.toEqual(stored);
    await settle();
    flush();
    expect(putSettings).toHaveBeenCalledWith(sent);
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
    expect(app.repos()).toEqual({ status: "ready", repos: [makeRepo("home", { home: true })] });
  });

  it("keeps the settings on screen when a save is refused", async () => {
    const stored: Settings = {
      home_repo: null,
      scan_roots: [],
      clone_root: null,
      max_parallel: null,
    };
    const refusal = badRequest("home_repo: /nope is not the root of a git work tree");
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([]);
    const { app } = setup({
      getSettings: () => Promise.resolve(stored),
      putSettings: () => Promise.reject(refusal),
      listRepos,
    });
    app.loadSettings();
    app.loadRepos();
    await settle();

    await expect(app.saveSettings({ ...stored, home_repo: "/nope" })).rejects.toBe(refusal);
    flush();
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
    expect(listRepos).toHaveBeenCalledTimes(1);
  });

  it("adds a registered repo and drops a removed one without reading the list again", async () => {
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([makeRepo("web")]);
    const registerRepo = vi.fn<Api["registerRepo"]>().mockResolvedValue(makeRepo("api"));
    const removeRepo = vi.fn<Api["removeRepo"]>().mockResolvedValue();
    const { app } = setup({ listRepos, registerRepo, removeRepo });
    const names = () => {
      flush();
      const registry = app.repos();
      return registry.status === "ready" ? registry.repos.map((repo) => repo.name) : registry;
    };
    app.loadRepos();
    await settle();

    await expect(app.registerRepo({ path: "/code/api" })).resolves.toEqual(makeRepo("api"));
    expect(registerRepo).toHaveBeenCalledWith({ path: "/code/api" });
    expect(names()).toEqual(["api", "web"]);

    await app.removeRepo("web");
    expect(removeRepo).toHaveBeenCalledWith("web");
    expect(names()).toEqual(["api"]);
    expect(listRepos).toHaveBeenCalledTimes(1);
  });

  it("keeps the registry when a removal is refused", async () => {
    const refusal = new Error("repo is attached to a running session");
    const { app } = setup({
      listRepos: () => Promise.resolve([makeRepo("api")]),
      removeRepo: () => Promise.reject(refusal),
    });
    app.loadRepos();
    await settle();

    await expect(app.removeRepo("api")).rejects.toBe(refusal);
    flush();
    expect(app.repos()).toEqual({ status: "ready", repos: [makeRepo("api")] });
  });

  it("reports a registry that cannot be read, and reads it once the socket is back", async () => {
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue([makeRepo("api")]);
    const { app, socket } = setup({ listRepos });
    app.loadRepos();
    await settle();
    flush();
    expect(app.repos()).toEqual({ status: "error", message: "Cannot reach the server" });

    socket.setState("open");
    await settle();
    flush();
    expect(app.repos()).toEqual({ status: "ready", repos: [makeRepo("api")] });
  });

  it("says so when the server refuses the sessions topic, and asks again on retry", async () => {
    const { app, socket } = setup();
    await settle();
    socket.emit("sessions", { op: "error", topic: "sessions", message: "not allowed" });
    flush();
    expect(app.sessionsError()).toBe("not allowed");

    app.retry();
    expect(socket.resyncs).toEqual([{ topic: "sessions", afterSeq: 0 }]);
    socket.emit("sessions", { op: "subscribed", topic: "sessions", head_seq: 0 });
    flush();
    expect(app.sessionsError()).toBeUndefined();
  });
});

describe("app state: workflows", () => {
  it("reads the workflows on demand, keeps them through a later failure, and reads again after a save", async () => {
    const listed = [workflowInfo("quick", { builtin: true }), workflowInfo("itr")];
    const listWorkflows = vi
      .fn<Api["listWorkflows"]>()
      .mockResolvedValueOnce(listed)
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue([listed[0]]);
    const putSettings = vi
      .fn<Api["putSettings"]>()
      .mockImplementation((body) => Promise.resolve(body));
    const { app, socket } = setup({
      listWorkflows,
      putSettings,
      listRepos: () => Promise.resolve([]),
    });
    expect(app.workflows()).toEqual({ status: "loading" });
    await settle();
    expect(listWorkflows).not.toHaveBeenCalled();

    app.loadWorkflows();
    await settle();
    flush();
    expect(app.workflows()).toEqual({ status: "ready", workflows: listed });

    // A reconnect reads them again; a failure then keeps the list on screen.
    socket.setState("open");
    socket.setState("reconnecting");
    socket.setState("open");
    await settle();
    flush();
    expect(listWorkflows).toHaveBeenCalledTimes(2);
    expect(app.workflows()).toEqual({ status: "ready", workflows: listed });

    // Another home repo, other workflows.
    await app.saveSettings({
      home_repo: "/code/b",
      scan_roots: [],
      clone_root: null,
      max_parallel: 2,
    });
    await settle();
    flush();
    expect(listWorkflows).toHaveBeenCalledTimes(3);
    expect(app.workflows()).toEqual({ status: "ready", workflows: [listed[0]] });
  });

  it("shows the server's message when the first read fails, and reads again on retry", async () => {
    const listWorkflows = vi
      .fn<Api["listWorkflows"]>()
      .mockRejectedValueOnce(badRequest("home repo has no .calin folder"))
      .mockResolvedValue([workflowInfo("quick")]);
    const { app } = setup({ listWorkflows });
    app.loadWorkflows();
    await settle();
    flush();
    expect(app.workflows()).toEqual({ status: "error", message: "home repo has no .calin folder" });
    app.retry();
    await settle();
    flush();
    expect(app.workflows()).toMatchObject({ status: "ready" });
  });
});

describe("app state: reads on their way", () => {
  it("asks once for settings, workflows and health while a read is on its way", async () => {
    const settingsRead = deferred<Settings>();
    const getSettings = vi.fn<Api["getSettings"]>().mockReturnValue(settingsRead.promise);
    const listWorkflows = vi.fn<Api["listWorkflows"]>().mockReturnValue(new Promise(() => {}));
    const read = vi.fn<Api["health"]>().mockReturnValue(new Promise(() => {}));
    const { app, socket } = setup({ getSettings, listWorkflows, health: read });
    await settle();

    app.loadSettings();
    app.loadSettings();
    app.loadWorkflows();
    app.loadWorkflows();
    // Opening the socket before health has answered would read it again.
    socket.setState("open");
    await settle();

    expect(getSettings).toHaveBeenCalledTimes(1);
    expect(listWorkflows).toHaveBeenCalledTimes(1);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("keeps settings read once on screen when a later read fails", async () => {
    const stored: Settings = { home_repo: null, scan_roots: [], clone_root: null, max_parallel: 1 };
    const getSettings = vi
      .fn<Api["getSettings"]>()
      .mockResolvedValueOnce(stored)
      .mockRejectedValue(unreachable());
    const { app } = setup({ getSettings });
    app.loadSettings();
    await settle();

    app.loadSettings();
    flush();
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
    await settle();
    flush();
    expect(getSettings).toHaveBeenCalledTimes(2);
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
  });

  it("reads on retry the settings that never arrived", async () => {
    const stored: Settings = { home_repo: null, scan_roots: [], clone_root: null, max_parallel: 1 };
    const getSettings = vi
      .fn<Api["getSettings"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(stored);
    const { app } = setup({ getSettings });
    await settle();
    app.loadSettings();
    await settle();
    flush();
    expect(app.settings()).toMatchObject({ status: "error" });

    app.retry();
    await settle();
    flush();
    expect(app.settings()).toEqual({ status: "ready", settings: stored });
  });

  it("passes a repo's issues and one issue through from the server", async () => {
    const issues = [makeIssue(7)];
    const issue = { issue: makeIssue(7), body: "Steps to reproduce" };
    const listIssues = vi.fn<Api["listIssues"]>().mockResolvedValue(issues);
    const getIssue = vi.fn<Api["getIssue"]>().mockResolvedValue(issue);
    const { app } = setup({ listIssues, getIssue });

    await expect(app.listIssues("web", { state: "open" })).resolves.toBe(issues);
    await expect(app.getIssue("web", 7)).resolves.toBe(issue);
    expect(listIssues).toHaveBeenCalledWith("web", { state: "open" });
    expect(getIssue).toHaveBeenCalledWith("web", 7);
  });
});

describe("app state: sign-in and the browser", () => {
  const publicMe: Me = { mode: "public", identity: "ada@example.org", device_token: false };

  afterEach(() => {
    Reflect.deleteProperty(navigator, "onLine");
    vi.useRealTimers();
  });

  it("asks the socket to reconnect when the page is shown again or the browser is back online", async () => {
    const { socket } = setup();
    await settle();
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(socket.reconnects).toBe(0);

    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(socket.reconnects).toBe(1);
    window.dispatchEvent(new Event("online"));
    expect(socket.reconnects).toBe(2);
    visibility.mockRestore();
  });

  it("follows the browser's network, and asks /me again only when the server was unreachable", async () => {
    const me = vi.fn<Api["me"]>().mockResolvedValue(publicMe);
    const { app } = setup({ me });
    await settle();
    expect(app.auth()).toEqual({ status: "signed_in", identity: "ada@example.org" });

    window.dispatchEvent(new Event("offline"));
    flush();
    expect(app.online()).toBe(false);
    window.dispatchEvent(new Event("online"));
    flush();
    expect(app.online()).toBe(true);
    expect(me).toHaveBeenCalledTimes(1);
  });

  it("takes a public answer without an identity as signed in with no name", async () => {
    const { app } = setup({ me: () => Promise.resolve({ ...publicMe, identity: null }) });
    await settle();
    flush();
    expect(app.auth()).toEqual({ status: "signed_in", identity: "" });
    expect(app.identity()).toBe("");
  });

  it("asks /me once while it is on its way, however often it is asked", async () => {
    const answer = deferred<Me>();
    const me = vi.fn<Api["me"]>().mockReturnValueOnce(answer.promise).mockResolvedValue(publicMe);
    const { app, socket } = setup({ me });
    app.retry();
    expect(me).toHaveBeenCalledTimes(1);

    answer.resolve(publicMe);
    await settle();
    // A refused upgrade asks /me once, and not again while that answer is on its way.
    const probe = deferred<Me>();
    me.mockReturnValueOnce(probe.promise);
    socket.refuse();
    socket.refuse();
    expect(me).toHaveBeenCalledTimes(2);
    probe.resolve(publicMe);
    await settle();
    socket.refuse();
    expect(me).toHaveBeenCalledTimes(3);
  });

  it("asks nothing more once stopped while /me was on its way", async () => {
    vi.useFakeTimers();
    const answer = deferred<Me>();
    const me = vi.fn<Api["me"]>().mockReturnValue(answer.promise);
    const { app, socket } = setup({ me });
    for (const stop of stops.splice(0)) stop();

    answer.reject(unreachable());
    await vi.advanceTimersByTimeAsync(60_000);
    flush();
    expect(me).toHaveBeenCalledTimes(1);
    expect(app.auth()).toEqual({ status: "checking" });
    expect(socket.subscriberCount("sessions")).toBe(0);
  });

  it("keeps a sign-out as it was when a later call is refused too, and retries nothing", async () => {
    const read = vi.fn<Api["health"]>().mockResolvedValue(health);
    const { app, socket } = setup({
      me: () => Promise.resolve(publicMe),
      health: read,
      logout: () => Promise.resolve(),
      listAuthSessions: () => Promise.reject(signedOut()),
    });
    await settle();
    await app.signOut();
    flush();
    expect(app.auth()).toEqual({ status: "signed_out", ended: false });

    await expect(app.listAuthSessions()).rejects.toThrow("Not signed in");
    flush();
    expect(app.auth()).toEqual({ status: "signed_out", ended: false });

    const reads = read.mock.calls.length;
    app.retry();
    expect(read).toHaveBeenCalledTimes(reads);
    expect(socket.reconnects).toBe(0);
  });

  it("reports nothing when the worker renewed a subscription this browser no longer has", async () => {
    const device = new FakeDevice();
    device.renewed = true;
    const createPushSubscription = vi.fn<Api["createPushSubscription"]>();
    setup({ me: () => Promise.resolve(publicMe), createPushSubscription }, { device });
    await settle();
    expect(device.renewed).toBe(false);
    expect(createPushSubscription).not.toHaveBeenCalled();
  });

  it("reports a renewed subscription in public mode only", async () => {
    const device = new FakeDevice();
    device.renewed = true;
    device.current = makeDeviceSubscription("hash-renewed");
    const createPushSubscription = vi.fn<Api["createPushSubscription"]>();
    setup({ createPushSubscription }, { device });
    await settle();
    expect(device.renewed).toBe(true);
    expect(createPushSubscription).not.toHaveBeenCalled();
  });
});

describe("what /me says", () => {
  it("is local mode, or signed in under the identity it names", () => {
    expect(meAuth({ mode: "local", identity: null, device_token: false })).toEqual({
      status: "local",
    });
    expect(meAuth({ mode: "public", identity: "ada@example.org", device_token: true })).toEqual({
      status: "signed_in",
      identity: "ada@example.org",
    });
  });

  it("leaves a 401 to the http layer, takes a server's 404 as local mode, and retries the rest", () => {
    expect(meFailure(signedOut())).toBe("handled");
    expect(meFailure(new HttpError(404, "not found", true))).toBe("local");
    expect(meFailure(new HttpError(404, "not found", false))).toBe("unreachable");
    expect(meFailure(unreachable())).toBe("unreachable");
    expect(meFailure(new Error("boom"))).toBe("unreachable");
  });

  it("waits twice as long after each failure, up to fifteen seconds", () => {
    expect([0, 1, 2, 3, 4, 10].map(meRetryDelay)).toEqual([1000, 2000, 4000, 8000, 15_000, 15_000]);
  });

  it("is pending until it has answered as Calin", () => {
    expect(authPending({ status: "checking" })).toBe(true);
    expect(authPending({ status: "unreachable" })).toBe(true);
    expect(authPending({ status: "local" })).toBe(false);
    expect(authPending({ status: "signed_out", ended: true })).toBe(false);
  });
});
