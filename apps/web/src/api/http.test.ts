import { describe, expect, it, vi } from "vite-plus/test";
import type { Health } from "../gen/Health";
import { makeDetail, makeRepo, makeSession } from "../test/fakes";
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
    const health: Health = { name: "calin", version: "0.1.0", dry_run: false, runners: ["mock"] };
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

  it("reports a bad path as the server words it", async () => {
    const { api } = setup(() =>
      Promise.resolve(json({ error: "/tmp/nope is not the root of a git work tree" }, 400)),
    );
    const error = await failureOf(api.registerRepo({ path: "/tmp/nope" }));
    expect(error.status).toBe(400);
    expect(error.message).toBe("/tmp/nope is not the root of a git work tree");
  });
});
