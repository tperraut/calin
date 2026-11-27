import { describe, expect, it, vi } from "vite-plus/test";
import type { Health } from "../gen/Health";
import type { RepoSet } from "../gen/RepoSet";
import type { Settings } from "../gen/Settings";
import { makeDetail, makeMissingRepo, makeRepo, makeSession, makeSessionRepo } from "../test/fakes";
import { makeGraph, makeTimeline } from "../test/orchestration";
import { createApi, HttpError, type FetchLike } from "./http";

const json = (body: unknown, status = 200): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

function setup(respond: FetchLike) {
  const fetchFn = vi.fn<FetchLike>(respond);
  return { api: createApi(fetchFn), fetchFn };
}

async function failureOf(promise: Promise<unknown>): Promise<HttpError> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof HttpError) return error;
    throw error;
  }
  throw new Error("the request was expected to fail");
}

describe("http api", () => {
  it("calls every route of the table with its method, path and body", async () => {
    const health: Health = {
      name: "calin",
      version: "0.1.0",
      dry_run: false,
      runners: ["mock"],
      github_problem: null,
    };
    const detail = makeDetail(makeSession("s 1"));
    const { api, fetchFn } = setup((input, init) => {
      if (input.endsWith("/health")) return Promise.resolve(json(health));
      if (input.endsWith("/sessions") && init?.method === "GET") {
        return Promise.resolve(json([detail.session]));
      }
      if (input.endsWith("/sessions")) return Promise.resolve(json(detail, 201));
      if (init?.method === "GET") return Promise.resolve(json(detail));
      return Promise.resolve(new Response(null, { status: 202 }));
    });

    await expect(api.health()).resolves.toEqual(health);
    await expect(api.listSessions()).resolves.toEqual([detail.session]);
    await expect(api.createSession({ prompt: "go", repos: [] })).resolves.toEqual(detail);
    await expect(api.getSession("s 1")).resolves.toEqual(detail);
    await expect(api.sendMessage("s 1", { text: "hi" })).resolves.toBeUndefined();
    await expect(api.stopSession("s 1")).resolves.toBeUndefined();

    const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
    expect(calls).toEqual([
      ["GET", "/api/v1/health", undefined],
      ["GET", "/api/v1/sessions", undefined],
      ["POST", "/api/v1/sessions", '{"prompt":"go","repos":[]}'],
      ["GET", "/api/v1/sessions/s%201", undefined],
      ["POST", "/api/v1/sessions/s%201/messages", '{"text":"hi"}'],
      ["POST", "/api/v1/sessions/s%201/stop", undefined],
    ]);
  });

  it("ships a session and asks for its pull requests to be looked at, both answered with no body", async () => {
    const { api, fetchFn } = setup(() => Promise.resolve(new Response(null, { status: 202 })));

    await expect(
      api.shipSession("s 1", { title: "Add a health check", body: "Closes #4", draft: false }),
    ).resolves.toBeUndefined();
    await expect(api.shipSession("s 1", {})).resolves.toBeUndefined();
    await expect(api.refreshPullRequests("s 1")).resolves.toBeUndefined();

    const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
    expect(calls).toEqual([
      [
        "POST",
        "/api/v1/sessions/s%201/ship",
        '{"title":"Add a health check","body":"Closes #4","draft":false}',
      ],
      // Every field is optional: the server fills in the session's title and opens drafts.
      ["POST", "/api/v1/sessions/s%201/ship", "{}"],
      ["POST", "/api/v1/sessions/s%201/pull-requests/refresh", undefined],
    ]);
  });

  it("gives the server's reason when a ship or a refresh is refused", async () => {
    const { api } = setup((input) =>
      Promise.resolve(
        input.endsWith("/ship")
          ? json({ error: "the gh CLI is not signed in" }, 409)
          : json({ error: "session not found" }, 404),
      ),
    );
    const refused = await failureOf(api.shipSession("s1", {}));
    expect([refused.status, refused.message, refused.fromServer]).toEqual([
      409,
      "the gh CLI is not signed in",
      true,
    ]);
    const missing = await failureOf(api.refreshPullRequests("nope"));
    expect([missing.status, missing.message]).toEqual([404, "session not found"]);
  });

  it("surfaces the server's own message from an ApiError", async () => {
    const { api } = setup(() => Promise.resolve(json({ error: "prompt must not be blank" }, 400)));
    const error = await failureOf(api.createSession({ prompt: " ", repos: [] }));
    expect(error.message).toBe("prompt must not be blank");
    expect(error.status).toBe(400);
    expect(error.fromServer).toBe(true);
    expect(error.unreachable).toBe(false);
  });

  it("keeps the status of a 404 and a 409", async () => {
    const { api } = setup((input) =>
      Promise.resolve(
        input.endsWith("/stop")
          ? json({ error: "session is already done" }, 409)
          : json({ error: "unknown session" }, 404),
      ),
    );
    expect((await failureOf(api.getSession("nope"))).status).toBe(404);
    const conflict = await failureOf(api.stopSession("s1"));
    expect(conflict.status).toBe(409);
    expect(conflict.message).toBe("session is already done");
  });

  it("reports a network failure as an unreachable server", async () => {
    const { api } = setup(() => Promise.reject(new TypeError("Failed to fetch")));
    const error = await failureOf(api.health());
    expect(error.message).toBe("Cannot reach the server");
    expect(error.status).toBe(0);
    expect(error.unreachable).toBe(true);
  });

  it("reports a proxy error without an ApiError body as an unreachable server", async () => {
    // What `vp dev` answers while `make dev-server` is not running.
    const { api } = setup(() => Promise.resolve(new Response("", { status: 500 })));
    const error = await failureOf(api.health());
    expect(error.message).toBe("Cannot reach the server");
    expect(error.unreachable).toBe(true);
  });

  it("does not mistake a server-side failure with an ApiError for an outage", async () => {
    const { api } = setup(() => Promise.resolve(json({ error: "database is locked" }, 500)));
    const error = await failureOf(api.health());
    expect(error.message).toBe("database is locked");
    expect(error.unreachable).toBe(false);
  });

  it("rejects a success that is not JSON", async () => {
    const { api } = setup(() => Promise.resolve(new Response("<!doctype html>", { status: 200 })));
    const error = await failureOf(api.health());
    expect(error.message).toBe("Unexpected response from the server");
  });

  it("calls the repo, interrupt and permission routes with their method, path and body", async () => {
    const repo = makeRepo("api");
    const { api, fetchFn } = setup((input, init) => {
      if (input.endsWith("/repos") && init?.method === "GET") return Promise.resolve(json([repo]));
      if (input.endsWith("/repos")) return Promise.resolve(json(repo, 201));
      if (init?.method === "DELETE") return Promise.resolve(new Response(null, { status: 204 }));
      return Promise.resolve(new Response(null, { status: 202 }));
    });

    await expect(api.listRepos()).resolves.toEqual([repo]);
    await expect(api.registerRepo({ path: "/code/api", name: "api" })).resolves.toEqual(repo);
    await expect(api.removeRepo("r 1")).resolves.toBeUndefined();
    await expect(api.interruptSession("s 1")).resolves.toBeUndefined();
    await expect(
      api.resolvePermission("s 1", "p/1", { decision: "deny", message: "Not on main." }),
    ).resolves.toBeUndefined();
    await expect(
      api.resolvePermission("s 1", "p2", { decision: "allow" }),
    ).resolves.toBeUndefined();

    const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
    expect(calls).toEqual([
      ["GET", "/api/v1/repos", undefined],
      ["POST", "/api/v1/repos", '{"path":"/code/api","name":"api"}'],
      ["DELETE", "/api/v1/repos/r%201", undefined],
      ["POST", "/api/v1/sessions/s%201/interrupt", undefined],
      [
        "POST",
        "/api/v1/sessions/s%201/permissions/p%2F1",
        '{"decision":"deny","message":"Not on main."}',
      ],
      ["POST", "/api/v1/sessions/s%201/permissions/p2", '{"decision":"allow"}'],
    ]);
  });

  it("calls the settings, catalog and attach routes with their method, path and body", async () => {
    const settings: Settings = {
      home_repo: "/code/home",
      scan_roots: ["/code"],
      clone_root: null,
    };
    const repos = [makeRepo("home", { home: true }), makeMissingRepo("web")];
    const sets: RepoSet[] = [{ name: "storefront", repo_ids: ["api", "web"] }];
    const attached = makeSessionRepo("web", { session_id: "s 1" });
    const { api, fetchFn } = setup((input) => {
      if (input.endsWith("/settings")) return Promise.resolve(json(settings));
      if (input.endsWith("/repos/refresh")) return Promise.resolve(json(repos));
      if (input.endsWith("/clone")) return Promise.resolve(new Response(null, { status: 202 }));
      if (input.endsWith("/repo-sets")) return Promise.resolve(json(sets));
      return Promise.resolve(json(attached, 201));
    });

    await expect(api.getSettings()).resolves.toEqual(settings);
    await expect(api.putSettings(settings)).resolves.toEqual(settings);
    await expect(api.refreshRepos()).resolves.toEqual(repos);
    await expect(api.cloneRepo("r/1")).resolves.toBeUndefined();
    await expect(api.listRepoSets()).resolves.toEqual(sets);
    await expect(api.attachRepo("s 1", { repo_id: "web", mode: "read" })).resolves.toEqual(
      attached,
    );

    const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
    expect(calls).toEqual([
      ["GET", "/api/v1/settings", undefined],
      [
        "PUT",
        "/api/v1/settings",
        '{"home_repo":"/code/home","scan_roots":["/code"],"clone_root":null}',
      ],
      ["POST", "/api/v1/repos/refresh", undefined],
      ["POST", "/api/v1/repos/r%2F1/clone", undefined],
      ["GET", "/api/v1/repo-sets", undefined],
      ["POST", "/api/v1/sessions/s%201/repos", '{"repo_id":"web","mode":"read"}'],
    ]);
  });

  it("surfaces the server's message when a settings, clone or attach route refuses", async () => {
    const answers: Record<string, [number, string]> = {
      "PUT /api/v1/settings": [400, "home_repo: /tmp/x is not the root of a git work tree"],
      "POST /api/v1/repos/web/clone": [409, "web has no url to clone from"],
      "DELETE /api/v1/repos/web": [409, "web is declared by the home repo"],
      "POST /api/v1/sessions/s1/repos": [409, "web is already attached"],
    };
    const { api } = setup((input, init) => {
      const [status, error] = answers[`${init?.method} ${input}`] ?? [500, "unexpected"];
      return Promise.resolve(json({ error }, status));
    });
    const refused = [
      await failureOf(api.putSettings({ home_repo: "/tmp/x", scan_roots: [], clone_root: null })),
      await failureOf(api.cloneRepo("web")),
      await failureOf(api.removeRepo("web")),
      await failureOf(api.attachRepo("s1", { repo_id: "web" })),
    ];
    expect(refused.map((error) => [error.status, error.message, error.fromServer])).toEqual([
      [400, "home_repo: /tmp/x is not the root of a git work tree", true],
      [409, "web has no url to clone from", true],
      [409, "web is declared by the home repo", true],
      [409, "web is already attached", true],
    ]);
  });

  it("surfaces the server's message when a new route refuses", async () => {
    const answers: Record<string, [number, string]> = {
      "POST /api/v1/repos": [409, "a repo named api is already registered"],
      "DELETE /api/v1/repos/r1": [409, "api is attached to a session that is still open"],
      "POST /api/v1/sessions/s1/interrupt": [409, "session is not running"],
      "POST /api/v1/sessions/s1/permissions/p1": [409, "request was already answered"],
      "POST /api/v1/sessions/s1/permissions/nope": [404, "unknown request"],
    };
    const { api } = setup((input, init) => {
      const [status, error] = answers[`${init?.method} ${input}`] ?? [500, "unexpected call"];
      return Promise.resolve(json({ error }, status));
    });

    const failures = await Promise.all([
      failureOf(api.registerRepo({ path: "/code/api" })),
      failureOf(api.removeRepo("r1")),
      failureOf(api.interruptSession("s1")),
      failureOf(api.resolvePermission("s1", "p1", { decision: "allow" })),
      failureOf(api.resolvePermission("s1", "nope", { decision: "allow" })),
    ]);
    expect(failures.map((error) => [error.status, error.message, error.fromServer])).toEqual([
      [409, "a repo named api is already registered", true],
      [409, "api is attached to a session that is still open", true],
      [409, "session is not running", true],
      [409, "request was already answered", true],
      [404, "unknown request", true],
    ]);
  });

  it("answers a question and reads the graph and the timeline of a session's tree", async () => {
    const graph = makeGraph();
    const timeline = makeTimeline();
    const { api, fetchFn } = setup((input, init) => {
      if (init?.method === "POST") return Promise.resolve(new Response(null, { status: 202 }));
      return Promise.resolve(json(input.includes("/graph") ? graph : timeline));
    });

    await expect(api.answerQuestion("s 1", "q/1", { answer: "Header" })).resolves.toBeUndefined();
    await expect(api.getGraph("s 1")).resolves.toEqual(graph);
    // `at` is Unix milliseconds, sent whole.
    await expect(api.getGraph("s 1", 1_800_000_012_345.6)).resolves.toEqual(graph);
    await expect(api.getTimeline("s 1")).resolves.toEqual(timeline);

    const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
    expect(calls).toEqual([
      ["POST", "/api/v1/sessions/s%201/questions/q%2F1", '{"answer":"Header"}'],
      ["GET", "/api/v1/sessions/s%201/graph", undefined],
      ["GET", "/api/v1/sessions/s%201/graph?at=1800000012346", undefined],
      ["GET", "/api/v1/sessions/s%201/timeline", undefined],
    ]);
  });

  it("gives the server's reason when a question cannot be answered or a graph cannot be read", async () => {
    const { api } = setup((input) =>
      Promise.resolve(
        input.includes("/questions/")
          ? json({ error: "question was already answered" }, 409)
          : json({ error: "session not found" }, 404),
      ),
    );
    const failures = await Promise.all([
      failureOf(api.answerQuestion("s1", "q1", { answer: "x" })),
      failureOf(api.getGraph("nope")),
      failureOf(api.getTimeline("nope")),
    ]);
    expect(failures.map((error) => [error.status, error.message, error.fromServer])).toEqual([
      [409, "question was already answered", true],
      [404, "session not found", true],
      [404, "session not found", true],
    ]);
  });

  it("reports a bad path as the server words it", async () => {
    const { api } = setup(() =>
      Promise.resolve(json({ error: "/tmp/nope is not the root of a git work tree" }, 400)),
    );
    const error = await failureOf(api.registerRepo({ path: "/tmp/nope" }));
    expect(error.status).toBe(400);
    expect(error.message).toBe("/tmp/nope is not the root of a git work tree");
  });

  describe("workspace routes", () => {
    const content = {
      path: "src/main.rs",
      text: "fn main() {}\n",
      binary: false,
      too_large: false,
      size: 13,
      hash: "h1",
    };

    it("calls changes, diff, tree and file with their method, path and body", async () => {
      const { api, fetchFn } = setup((input) =>
        Promise.resolve(json(input.includes("/file") ? content : [])),
      );

      await expect(api.getChanges("s 1")).resolves.toEqual([]);
      await api.getDiff("s 1", "sr/1", "src/main.rs");
      await api.listTree("s 1", "sr1", "");
      await api.listTree("s 1", "sr1", "src/api");
      await expect(api.readFile("s 1", "sr1", "src/main.rs")).resolves.toEqual(content);
      await expect(
        api.writeFile("s 1", "sr1", "src/main.rs", { text: "x", expected_hash: "h1" }),
      ).resolves.toEqual(content);
      await api.writeFile("s 1", "sr1", "notes.txt", { text: "" });

      const calls = fetchFn.mock.calls.map(([input, init]) => [init?.method, input, init?.body]);
      expect(calls).toEqual([
        ["GET", "/api/v1/sessions/s%201/changes", undefined],
        ["GET", "/api/v1/sessions/s%201/repos/sr%2F1/diff?path=src%2Fmain.rs", undefined],
        // api.rs: an absent `path` lists the root.
        ["GET", "/api/v1/sessions/s%201/repos/sr1/tree", undefined],
        ["GET", "/api/v1/sessions/s%201/repos/sr1/tree?path=src%2Fapi", undefined],
        ["GET", "/api/v1/sessions/s%201/repos/sr1/file?path=src%2Fmain.rs", undefined],
        [
          "PUT",
          "/api/v1/sessions/s%201/repos/sr1/file?path=src%2Fmain.rs",
          '{"text":"x","expected_hash":"h1"}',
        ],
        // Creating a file: no `expected_hash` at all.
        ["PUT", "/api/v1/sessions/s%201/repos/sr1/file?path=notes.txt", '{"text":""}'],
      ]);
      expect(fetchFn.mock.calls[5][1]?.headers).toMatchObject({
        "Content-Type": "application/json",
      });
    });

    it.each([
      "a file with spaces.txt",
      "notes#1.md",
      "what?.txt",
      "100%.txt",
      "50%25 off.txt",
      "a&b=c.txt",
      "a+b.txt",
      "dossier/été/naïve—文件.txt",
      "line\nbreak.txt",
      "tab\there.txt",
      "back\\slash.txt",
    ])("sends the path %j as one query value that decodes back to itself", async (path) => {
      const { api, fetchFn } = setup(() => Promise.resolve(json(content)));
      await api.readFile("s1", "sr1", path);
      await api.getDiff("s1", "sr1", path);
      await api.listTree("s1", "sr1", path);
      await api.writeFile("s1", "sr1", path, { text: "" });

      for (const [input] of fetchFn.mock.calls) {
        const url = new URL(input, "http://calin.test");
        // Exactly one parameter, and nothing of the path leaked into the address or a fragment.
        expect([...url.searchParams.keys()]).toEqual(["path"]);
        expect(url.searchParams.get("path")).toBe(path);
        expect(url.hash).toBe("");
        expect(url.pathname).toMatch(/^\/api\/v1\/sessions\/s1\/repos\/sr1\/(file|diff|tree)$/);
        expect(input).not.toMatch(/[\s#]/);
      }
    });

    it("surfaces the server's message and status when a workspace route refuses", async () => {
      const answers: Record<string, [number, string]> = {
        "GET /api/v1/sessions/s1/changes": [404, "unknown session"],
        "GET /api/v1/sessions/s1/repos/sr1/diff?path=..%2Fsecret": [400, "path leaves the repo"],
        "GET /api/v1/sessions/s1/repos/sr1/tree?path=nope": [404, "nope is not a directory"],
        "GET /api/v1/sessions/s1/repos/sr1/file?path=.git%2Fconfig": [400, ".git is never read"],
        "PUT /api/v1/sessions/s1/repos/sr1/file?path=a.txt": [409, "a.txt changed on disk"],
      };
      const { api } = setup((input, init) => {
        const [status, error] = answers[`${init?.method} ${input}`] ?? [500, "unexpected call"];
        return Promise.resolve(json({ error }, status));
      });

      const failures = await Promise.all([
        failureOf(api.getChanges("s1")),
        failureOf(api.getDiff("s1", "sr1", "../secret")),
        failureOf(api.listTree("s1", "sr1", "nope")),
        failureOf(api.readFile("s1", "sr1", ".git/config")),
        failureOf(api.writeFile("s1", "sr1", "a.txt", { text: "x", expected_hash: "old" })),
      ]);
      expect(failures.map((error) => [error.status, error.message, error.fromServer])).toEqual([
        [404, "unknown session", true],
        [400, "path leaves the repo", true],
        [404, "nope is not a directory", true],
        [400, ".git is never read", true],
        [409, "a.txt changed on disk", true],
      ]);
    });

    it("reports an unreachable server for a save like for any other call", async () => {
      const { api } = setup(() => Promise.reject(new TypeError("Failed to fetch")));
      const error = await failureOf(api.writeFile("s1", "sr1", "a.txt", { text: "x" }));
      expect(error.unreachable).toBe(true);
      expect(error.message).toBe("Cannot reach the server");
    });
  });
});
