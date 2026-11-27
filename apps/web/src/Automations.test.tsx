import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Automation } from "./gen/Automation";
import type { AutomationRequest } from "./gen/AutomationRequest";
import type { CronPreview } from "./gen/CronPreview";
import type { Health } from "./gen/Health";
import { CRON_PREVIEW_DELAY_MS } from "./pages/AutomationPage";
import { createAppState } from "./state/app";
import {
  badRequest,
  conflict,
  fakeApi,
  FakeSocket,
  immediately,
  makeRepo,
  settle,
} from "./test/fakes";
import { makeAutomation, makeRun } from "./test/integrations";
import { click, control, find, findByText, mount, type, unmountAll } from "./test/render";

afterEach(() => {
  vi.useRealTimers();
  unmountAll();
});

const health: Health = {
  name: "calin",
  version: "1",
  dry_run: false,
  runners: ["claude", "mock"],
  github_problem: null,
};
const repos = [makeRepo("notes", { home: true }), makeRepo("api"), makeRepo("web")];
const MIN = 60_000;
const HOUR = 60 * MIN;

const schedule = makeAutomation("a1", {
  name: "Nightly check",
  next_run_at: Date.now() + 3 * HOUR + MIN,
  last_run: makeRun("r1", { at: Date.now() - HOUR, session_id: "s7", session_status: "review" }),
});
const issueTrigger = makeAutomation("a2", {
  name: "Triage",
  trigger: { kind: "issue", repo_id: "api", label: "agent" },
  trigger_text: "New issue labelled `agent` in api",
  last_poll_at: Date.now() - 2 * MIN,
});
const prTrigger = makeAutomation("a3", {
  name: "Review PRs",
  enabled: false,
  trigger: { kind: "pull_request", repo_id: "web" },
  trigger_text: "New pull request in web",
  last_poll_at: Date.now() - 20 * MIN,
  last_poll_problem: "gh: authentication required",
  disabled_reason: "3 runs in a row failed",
  last_run: makeRun("r3", {
    outcome: "failed",
    cause: "pull request #7",
    session_id: null,
    session_status: null,
    detail: "the repo web is not available",
  }),
});

async function open(path: string, routes: Partial<Api> = {}) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve(repos),
    listAutomations: () => Promise.resolve([schedule, issueTrigger, prTrigger]),
    getAutomation: (id) =>
      Promise.resolve([schedule, issueTrigger, prTrigger].find((one) => one.id === id) ?? schedule),
    listAutomationRuns: () => Promise.resolve([]),
    previewCron: () => Promise.resolve({ text: "Every weekday at 08:00", next: [], problem: null }),
    ...routes,
  });
  const state = createAppState({ api, socket, schedule: immediately });
  const host = mount(() => <App state={state} />);
  socket.setState("open");
  const waiting = () => {
    const shown = host.querySelector("main")?.textContent;
    return shown === undefined || shown === "Loading…";
  };
  for (let i = 0; i < 50 && waiting(); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 5));
    flush();
  }
  await settle();
  flush();
  const main = () => find(host, "main");
  return { host, main, api };
}

const text = (element: Element | null | undefined): string =>
  (element?.textContent ?? "").replace(/\s+/g, " ").trim();

describe("Automations list", () => {
  it("shows each kind of trigger with its state, its switch and its last run", async () => {
    const { main } = await open("/automations");
    const rows = [...main().querySelectorAll('[data-part="automation"]')];
    expect(rows).toHaveLength(3);
    const [first, second, third] = rows.map(text);
    expect(first).toContain("Nightly check");
    expect(first).toContain("Every weekday at 08:00");
    expect(first).toContain("Next run in 3 h");
    expect(first).toContain("Last run 1 h ago: Started a session");
    expect(rows[0]?.querySelector('a[href="/s/s7"]')?.textContent).toContain("Review");
    expect(second).toContain("New issue labelled `agent` in api");
    expect(second).toContain("Checked 2 min ago");
    expect(third).toContain("GitHub could not be asked: gh: authentication required");
    expect(third).toContain("Switched off by Calin: 3 runs in a row failed");
    expect(third).toContain("Failed: the repo web is not available");
    const switches = [...main().querySelectorAll('[role="switch"]')];
    expect(
      switches.map((one) => [one.getAttribute("aria-label"), one.getAttribute("aria-checked")]),
    ).toEqual([
      ["Nightly check enabled", "true"],
      ["Triage enabled", "true"],
      ["Review PRs enabled", "false"],
    ]);
    // The next time exactly, on hover.
    expect(rows[0]?.querySelector("time")?.getAttribute("title")).not.toBe("");
  });

  it("explains what an automation is when there is none", async () => {
    const { main } = await open("/automations", { listAutomations: () => Promise.resolve([]) });
    expect(text(find(main(), '[data-part="empty"]'))).toContain(
      "An automation starts a session by itself, on a schedule or when a new issue or pull request appears on GitHub.",
    );
    expect(findByText(main(), "a", "New automation").getAttribute("href")).toBe("/automations/new");
  });

  it("saves the switch at once with the whole automation", async () => {
    const putAutomation = vi.fn((_id: string, body: AutomationRequest) =>
      Promise.resolve({ ...issueTrigger, enabled: body.enabled }),
    );
    const { main } = await open("/automations", { putAutomation });
    await click(find(main(), '[aria-label="Triage enabled"]'));
    expect(putAutomation.mock.calls).toEqual([
      [
        "a2",
        {
          name: "Triage",
          enabled: false,
          trigger: issueTrigger.trigger,
          session: issueTrigger.session,
        },
      ],
    ]);
    expect(find(main(), '[aria-label="Triage enabled"]').getAttribute("aria-checked")).toBe(
      "false",
    );
  });

  it("puts the switch back and shows the server's reason when the save fails", async () => {
    const { main } = await open("/automations", {
      putAutomation: () => Promise.reject(badRequest("the repo web is not available")),
    });
    const toggle = () => find(main(), '[aria-label="Review PRs enabled"]');
    await click(toggle());
    await settle();
    flush();
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(text(find(main(), '[role="alert"]'))).toBe("the repo web is not available");
  });
});

describe("Automation editor", () => {
  async function create(fill: (main: HTMLElement) => Promise<void> | void) {
    const createAutomation = vi.fn((body: AutomationRequest) =>
      Promise.resolve<Automation>({ ...makeAutomation("a9"), ...body }),
    );
    const view = await open("/automations/new", { createAutomation });
    type(control(view.main(), "Name"), "Weekly");
    type(control<HTMLTextAreaElement>(view.main(), "Prompt"), "Do {{date}}");
    await fill(view.main());
    await click(findByText(view.main(), "button", "Create automation"));
    await settle();
    return { ...view, body: createAutomation.mock.lastCall?.[0] };
  }

  it("sends a schedule, off by default, unattended, on the server's runner", async () => {
    const { body } = await create(async (main) => {
      await click(findByText(main, "button", "Every Monday at 09:00"));
    });
    expect(body).toEqual({
      name: "Weekly",
      enabled: false,
      trigger: { kind: "schedule", cron: "0 9 * * 1" },
      session: {
        prompt: "Do {{date}}",
        workflow: null,
        repos: [],
        runner: null,
        model: null,
        permission_mode: "accept_edits",
      },
    });
  });

  it("sends an issue trigger with its repo and label, and the chosen session options", async () => {
    const { body } = await create(async (main) => {
      await click(control(main, "New issue"));
      const repo = control<HTMLSelectElement>(main, "Repo");
      repo.value = "web";
      repo.dispatchEvent(new Event("change", { bubbles: true }));
      type(control(main, "Label"), " agent ");
      await click(control(main, "Autonomous"));
      await click(control(main, "Enabled"));
      const runner = control<HTMLSelectElement>(main, "Runner");
      runner.value = "mock";
      runner.dispatchEvent(new Event("change", { bubbles: true }));
      type(control(main, "Model"), "opus");
      flush();
    });
    expect(body).toMatchObject({
      enabled: true,
      trigger: { kind: "issue", repo_id: "web", label: "agent" },
      session: { runner: "mock", model: "opus", permission_mode: "autonomous" },
    });
  });

  it("sends a pull request trigger on the first repo unless another is chosen", async () => {
    const { body } = await create(async (main) => {
      await click(control(main, "New pull request"));
    });
    expect(body?.trigger).toEqual({ kind: "pull_request", repo_id: "notes" });
  });

  it("lists the placeholders of the chosen trigger, and does not offer Ask, saying why", async () => {
    const { main } = await open("/automations/new");
    const note = () => text(find(main(), '[data-part="placeholders"]'));
    expect(note()).toBe("You can use {{date}}, the local date.");
    await click(control(main(), "New issue"));
    expect(note()).toBe(
      "You can use {{date}}, the local date, and {{number}}, {{title}}, {{body}}, {{url}} and {{author}}, which describe the issue.",
    );
    const modes = [...main().querySelectorAll('fieldset input[type="radio"][value]')].map(
      (one) => (one as HTMLInputElement).value,
    );
    expect(modes).toContain("accept_edits");
    expect(modes).toContain("autonomous");
    expect(modes).not.toContain("ask");
    expect(main().textContent).toContain("An automated session runs unattended");
  });

  it("previews the cron expression once typing pauses: its words and next times, or the problem", async () => {
    const next = [Date.now() + HOUR, Date.now() + 2 * HOUR, Date.now() + 3 * HOUR];
    const previewCron = vi.fn((expr: string): Promise<CronPreview> =>
      Promise.resolve(
        expr.split(" ").length === 5
          ? { text: `Words for ${expr}`, next, problem: null }
          : { text: null, next: [], problem: "expected 5 fields" },
      ),
    );
    const { main } = await open("/automations/new", { previewCron });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const preview = () => text(find(main(), '[data-part="cron-preview"]'));
    type(control(main(), "Cron expression"), "0 8");
    expect(preview()).toBe("Checking the expression…");
    vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS);
    await settle();
    flush();
    expect(preview()).toBe("expected 5 fields");

    previewCron.mockClear();
    await click(findByText(main(), "button", "Every hour"));
    vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS - 1);
    expect(previewCron).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    await settle();
    flush();
    expect(previewCron.mock.calls).toEqual([["0 * * * *"]]);
    expect(preview()).toContain("Words for 0 * * * *");
    expect(find(main(), '[data-part="cron-preview"]').querySelectorAll("li")).toHaveLength(3);
    expect(find(main(), '[aria-label="Cron presets"] [aria-pressed="true"]').textContent).toBe(
      "Every hour",
    );

    previewCron.mockImplementation(() => Promise.reject(new Error("Cannot reach the server")));
    type(control(main(), "Cron expression"), "5 * * * *");
    vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS);
    await settle();
    flush();
    expect(preview()).toBe("The expression could not be checked: Cannot reach the server");
  });

  it("shows a 400 at the field it is about and focuses that field", async () => {
    const { main } = await open("/automations/new", {
      createAutomation: () => Promise.reject(badRequest("cron expression: expected 5 fields")),
    });
    type(control(main(), "Name"), "Weekly");
    await click(findByText(main(), "button", "Create automation"));
    await settle();
    flush();
    const cron = control(main(), "Cron expression");
    expect(document.activeElement).toBe(cron);
    expect(cron.getAttribute("aria-invalid")).toBe("true");
    const described = document.getElementById(
      cron.getAttribute("aria-describedby")?.split(" ").at(-1) ?? "",
    );
    expect(text(described)).toBe("cron expression: expected 5 fields");
  });

  it("shows a 400 it cannot place by Save, and focuses the message", async () => {
    const { main } = await open("/automations/new", {
      createAutomation: () => Promise.reject(badRequest("something else entirely")),
    });
    await click(findByText(main(), "button", "Create automation"));
    await settle();
    flush();
    const alert = find(main(), '[role="alert"]');
    expect(text(alert)).toBe("something else entirely");
    expect(document.activeElement).toBe(alert);
  });

  it("deletes only after a confirmation that says the sessions are kept", async () => {
    const deleteAutomation = vi.fn(() => Promise.resolve());
    const { main } = await open("/automations/a1", { deleteAutomation });
    await click(findByText(main(), "button", "Delete"));
    const confirm = find(main(), '[aria-label="Delete this automation"]');
    expect(text(confirm)).toContain("The sessions it started are kept.");
    await click(findByText(confirm, "button", "Keep"));
    expect(main().querySelector('[aria-label="Delete this automation"]')).toBeNull();
    expect(deleteAutomation).not.toHaveBeenCalled();

    await click(findByText(main(), "button", "Delete"));
    await click(findByText(main(), "button", "Delete automation"));
    await settle();
    expect(deleteAutomation).toHaveBeenCalledWith("a1");
    expect(location.pathname).toBe("/automations");
  });
});

describe("Run now and the history", () => {
  it("runs a schedule at once and links the started session", async () => {
    const runAutomation = vi.fn(() => Promise.resolve(makeRun("r9", { session_id: "s9" })));
    const { main } = await open("/automations/a1", { runAutomation });
    await click(findByText(main(), "button", "Run now"));
    expect(runAutomation).toHaveBeenCalledWith("a1", {});
    expect(findByText(main(), "a", "Open the started session").getAttribute("href")).toBe("/s/s9");
  });

  it("asks a trigger for the number first, and shows a 409", async () => {
    const runAutomation = vi
      .fn<Api["runAutomation"]>()
      .mockRejectedValueOnce(conflict("its previous session is still active"))
      .mockResolvedValueOnce(makeRun("r9", { session_id: "s9", cause: "issue #15" }));
    const { main } = await open("/automations/a2", { runAutomation });
    await click(findByText(main(), "button", "Run now"));
    expect(runAutomation).not.toHaveBeenCalled();
    type(control(main(), "Issue number"), "#12");
    await click(findByText(main(), "button", "Run"));
    expect(runAutomation).toHaveBeenLastCalledWith("a2", { number: 12 });
    expect(text(find(main(), '[aria-label="Run now"] [role="alert"]'))).toBe(
      "its previous session is still active",
    );
    type(control(main(), "Issue number"), "15");
    await click(findByText(main(), "button", "Run"));
    expect(runAutomation).toHaveBeenLastCalledWith("a2", { number: 15 });
    expect(findByText(main(), "a", "Open the started session").getAttribute("href")).toBe("/s/s9");
  });

  it("asks a pull request trigger for its number", async () => {
    const { main } = await open("/automations/a3");
    await click(findByText(main(), "button", "Run now"));
    expect(control(main(), "Pull request number")).toBeTruthy();
  });

  it("says what each past fire did", async () => {
    const at = Date.now() - 2 * HOUR;
    const { main } = await open("/automations/a1", {
      listAutomationRuns: () =>
        Promise.resolve([
          makeRun("h1", { at, cause: "manual", session_id: "s5", session_status: "done" }),
          makeRun("h2", {
            at,
            cause: "issue #12",
            outcome: "skipped",
            session_id: null,
            session_status: null,
            detail: "the previous session is still active",
          }),
          makeRun("h3", {
            at,
            outcome: "failed",
            session_id: null,
            session_status: null,
            detail: "unknown workflow review",
          }),
        ]),
    });
    const runs = [...find(main(), '[data-part="runs"]').querySelectorAll("li")].map(text);
    expect(runs).toEqual([
      "2 h ago·ManualStarted a sessionOpen the sessionDone",
      "2 h ago·Issue #12Skipped: the previous session is still active",
      "2 h ago·ScheduleFailed: unknown workflow review",
    ]);
  });

  it("says when it has not run yet", async () => {
    const { main } = await open("/automations/a1");
    expect(main().textContent).toContain("It has not run yet.");
  });
});
