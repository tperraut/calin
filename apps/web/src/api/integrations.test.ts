import { describe, expect, it, vi } from "vite-plus/test";
import { makeAutomation, makeIssue, makeLimits, makeRun, makeUsage } from "../test/integrations";
import { createApi, HttpError, type FetchLike } from "./http";

const json = (body: unknown, status = 200): Response =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

function setup(answer: (input: string, init?: RequestInit) => Response) {
  const fetchFn = vi.fn<FetchLike>((input, init) => Promise.resolve(answer(input, init)));
  const calls = () =>
    fetchFn.mock.calls.map(([input, init]) => [
      init?.method,
      input,
      init?.body === undefined ? undefined : JSON.parse(init.body as string),
    ]);
  return { api: createApi(fetchFn), calls };
}

describe("http api: usage, issues and automations", () => {
  it("calls the twelve routes with their method, path, query and body", async () => {
    const automation = makeAutomation("a 1");
    const run = makeRun("r1");
    const issue = makeIssue(12);
    const usage = makeUsage("day", []);
    const limits = [makeLimits("claude", [])];
    const preview = { text: "Every day", next: [1, 2, 3], problem: null };
    const { api, calls } = setup((input, init) => {
      const method = init?.method;
      if (input.includes("/issues/")) return json({ issue, body: "text" });
      if (input.includes("/issues")) return json([issue]);
      if (input.includes("/usage/limits")) return json(limits);
      if (input.includes("/usage")) return json(usage);
      if (input.includes("/cron")) return json(preview);
      if (input.endsWith("/runs")) return json([run]);
      if (input.endsWith("/run")) return json(run, 201);
      if (method === "DELETE") return new Response(null, { status: 204 });
      if (input.endsWith("/automations") && method === "GET") return json([automation]);
      return json(automation, method === "POST" ? 201 : 200);
    });
    const request = {
      name: "n",
      enabled: false,
      trigger: automation.trigger,
      session: automation.session,
    };

    await expect(
      api.listIssues("api 1", { state: "closed", q: "retry & back/off" }),
    ).resolves.toEqual([issue]);
    await expect(api.listIssues("api")).resolves.toEqual([issue]);
    await expect(api.listIssues("api", { q: "" })).resolves.toEqual([issue]);
    await expect(api.getIssue("api", 12)).resolves.toEqual({ issue, body: "text" });
    await expect(api.getUsage({ from: 10.4, to: 20, by: "session" })).resolves.toEqual(usage);
    await expect(api.getUsage()).resolves.toEqual(usage);
    await expect(api.getUsageLimits()).resolves.toEqual(limits);
    await expect(api.listAutomations()).resolves.toEqual([automation]);
    await expect(api.createAutomation(request)).resolves.toEqual(automation);
    await expect(api.previewCron("0 8 * * 1-5")).resolves.toEqual(preview);
    await expect(api.getAutomation("a 1")).resolves.toEqual(automation);
    await expect(api.putAutomation("a 1", request)).resolves.toEqual(automation);
    await expect(api.deleteAutomation("a 1")).resolves.toBeUndefined();
    await expect(api.runAutomation("a 1", { number: 7 })).resolves.toEqual(run);
    await expect(api.runAutomation("a 1", {})).resolves.toEqual(run);
    await expect(api.listAutomationRuns("a 1")).resolves.toEqual([run]);

    expect(calls()).toEqual([
      ["GET", "/api/v1/repos/api%201/issues?state=closed&q=retry%20%26%20back%2Foff", undefined],
      ["GET", "/api/v1/repos/api/issues", undefined],
      ["GET", "/api/v1/repos/api/issues", undefined],
      ["GET", "/api/v1/repos/api/issues/12", undefined],
      ["GET", "/api/v1/usage?from=10&to=20&by=session", undefined],
      ["GET", "/api/v1/usage", undefined],
      ["GET", "/api/v1/usage/limits", undefined],
      ["GET", "/api/v1/automations", undefined],
      ["POST", "/api/v1/automations", request],
      ["GET", "/api/v1/automations/cron?expr=0%208%20*%20*%201-5", undefined],
      ["GET", "/api/v1/automations/a%201", undefined],
      ["PUT", "/api/v1/automations/a%201", request],
      ["DELETE", "/api/v1/automations/a%201", undefined],
      ["POST", "/api/v1/automations/a%201/run", { number: 7 }],
      ["POST", "/api/v1/automations/a%201/run", {}],
      ["GET", "/api/v1/automations/a%201/runs", undefined],
    ]);
  });

  it("surfaces the server's message on a 400 and a 409", async () => {
    const { api } = setup((input) =>
      input.includes("/issues")
        ? json({ error: "api has no GitHub remote" }, 409)
        : json({ error: "cron expression: expected 5 fields" }, 400),
    );
    const refused = await api.listIssues("api").catch((error: unknown) => error);
    expect(refused).toBeInstanceOf(HttpError);
    expect(refused).toMatchObject({
      status: 409,
      message: "api has no GitHub remote",
      fromServer: true,
    });
    const invalid = await api
      .createAutomation({
        name: "x",
        enabled: false,
        trigger: { kind: "schedule", cron: "0 8" },
        session: {
          prompt: "p",
          workflow: null,
          repos: [],
          runner: null,
          model: null,
          permission_mode: "autonomous",
        },
      })
      .catch((error: unknown) => error);
    expect(invalid).toMatchObject({ status: 400, message: "cron expression: expected 5 fields" });
    await expect(api.runAutomation("a", {})).rejects.toMatchObject({ status: 400 });
  });
});
