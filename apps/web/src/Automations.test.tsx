import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Automation } from "./gen/Automation";
import type { AutomationRequest } from "./gen/AutomationRequest";
import type { CronPreview } from "./gen/CronPreview";
import type { Health } from "./gen/Health";
import { QUICK_WORKFLOW } from "./lib/workflow";
import { CRON_PREVIEW_DELAY_MS } from "./pages/AutomationPage";
import { createAppState } from "./state/app";
import {
  badRequest,
  conflict,
  deferred,
  fakeApi,
  FakeSocket,
  immediately,
  makeRepo,
  quickWorkflow,
  settle,
} from "./test/fakes";
import { makeAutomation, makeRun } from "./test/integrations";
import { workflowInfo } from "./test/workflows";
import {
  choose,
  click,
  control,
  find,
  findByText,
  mount,
  pageShown,
  type,
  unmountAll,
  until,
} from "./test/render";

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
  await until(() => pageShown(host));
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

  it.each(["write", "read", "live"] as const)(
    "saves a session repo attached as %s back as it was loaded",
    async (mode) => {
      const stored = makeAutomation("a5", {
        name: "Keep modes",
        session: {
          ...schedule.session,
          repos: [
            { repo_id: "api", mode },
            { repo_id: "web", mode: "read" },
            { repo_id: "notes", mode: "live" },
          ],
        },
      });
      const putAutomation = vi.fn((_id: string, body: AutomationRequest) =>
        Promise.resolve({ ...stored, ...body }),
      );
      const { main } = await open("/automations/a5", {
        getAutomation: () => Promise.resolve(stored),
        putAutomation,
      });
      await click(findByText(main(), "button", "Save"));
      await settle();
      expect(putAutomation.mock.calls).toEqual([
        [
          "a5",
          {
            name: "Keep modes",
            enabled: true,
            trigger: stored.trigger,
            session: {
              prompt: "Check {{date}}",
              workflow: null,
              repos: [
                { repo_id: "api", mode },
                { repo_id: "web", mode: "read" },
                { repo_id: "notes", mode: "live" },
              ],
              runner: null,
              model: null,
              permission_mode: "accept_edits",
            },
          },
        ],
      ]);
    },
  );

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

  it("opens to the new automation once it is created", async () => {
    const { main } = await create(() => undefined);
    expect(location.pathname).toBe("/automations/a9");
    await until(() => main().querySelector('[aria-label="Run now"]') !== null);
  });

  it("edits a trigger as it was saved, and sends no label once the label is cleared", async () => {
    const putAutomation = vi.fn((_id: string, body: AutomationRequest) =>
      Promise.resolve({ ...issueTrigger, ...body }),
    );
    const { main } = await open("/automations/a2", { putAutomation });
    expect(control<HTMLSelectElement>(main(), "Repo").value).toBe("api");
    expect(control(main(), "Label").value).toBe("agent");
    expect(text(find(main(), '[data-part="placeholders"]'))).toContain("which describe the issue.");
    expect(main().querySelector('[data-part="cron-preview"]')).toBeNull();
    type(control(main(), "Label"), "  ");
    await click(findByText(main(), "button", "Save"));
    await settle();
    expect(putAutomation.mock.lastCall?.[1].trigger).toEqual({
      kind: "issue",
      repo_id: "api",
      label: null,
    });
  });

  it("keeps a saved workflow, runner and model, and puts an unattended-unsafe mode back to Accept edits", async () => {
    const stored = makeAutomation("a6", {
      name: "Reviewer",
      session: {
        ...schedule.session,
        workflow: "review",
        runner: "mock",
        model: "opus",
        permission_mode: "ask",
      },
    });
    const putAutomation = vi.fn((_id: string, body: AutomationRequest) =>
      Promise.resolve({ ...stored, ...body }),
    );
    const { main } = await open("/automations/a6", {
      getAutomation: () => Promise.resolve(stored),
      listWorkflows: () => Promise.resolve([quickWorkflow, workflowInfo("review")]),
      putAutomation,
    });
    expect(control<HTMLInputElement>(main(), "Accept edits").checked).toBe(true);
    await click(findByText(main(), "button", "Save"));
    await settle();
    expect(putAutomation.mock.lastCall?.[1].session).toMatchObject({
      workflow: "review",
      runner: "mock",
      model: "opus",
      permission_mode: "accept_edits",
    });
    choose(control<HTMLSelectElement>(main(), "Workflow"), QUICK_WORKFLOW);
    await click(findByText(main(), "button", "Save"));
    await settle();
    expect(putAutomation.mock.lastCall?.[1].session.workflow).toBeNull();
  });

  it("says Saved and renames the page after a save, until the form changes again", async () => {
    const { main } = await open("/automations/a1", {
      putAutomation: (_id, body) => Promise.resolve({ ...schedule, ...body }),
    });
    type(control(main(), "Name"), "Morning check");
    await click(findByText(main(), "button", "Save"));
    await settle();
    flush();
    expect(text(find(main(), '[role="status"]'))).toBe("Saved");
    expect(text(find(main(), "h1"))).toBe("Morning check");
    type(control(main(), "Name"), "Morning check!");
    expect(main().querySelector('[role="status"]')).toBeNull();
  });

  it("sends one request while a save is under way, and says so on the button", async () => {
    const createAutomation = vi.fn(() => new Promise<Automation>(() => undefined));
    const { main } = await open("/automations/new", { createAutomation });
    const form = find<HTMLFormElement>(main(), 'form[aria-label="New automation"]');
    form.requestSubmit();
    flush();
    form.requestSubmit();
    flush();
    expect(createAutomation).toHaveBeenCalledTimes(1);
    const button = findByText<HTMLButtonElement>(main(), "button", "Saving…");
    expect(button.disabled).toBe(true);
  });

  it("offers no repo to watch when none is registered", async () => {
    const { main } = await open("/automations/new", { listRepos: () => Promise.resolve([]) });
    await click(control(main(), "New pull request"));
    const repo = control<HTMLSelectElement>(main(), "Repo");
    expect([...repo.options].map((one) => [one.value, one.textContent])).toEqual([
      ["", "No repo is registered"],
    ]);
    expect(text(repo.parentElement)).toContain("Fires once for each new open pull request");
  });

  it("says an expression is not valid when the server gives no reason", async () => {
    const { main } = await open("/automations/new", {
      previewCron: () => Promise.resolve({ text: null, next: [], problem: null }),
    });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    type(control(main(), "Cron expression"), "nonsense");
    vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS);
    await settle();
    flush();
    expect(text(find(main(), '[data-part="cron-preview"]'))).toBe(
      "This is not a valid cron expression.",
    );
  });

  it("shows only the answer about the expression last typed, and nothing for an empty one", async () => {
    const pending = new Map<string, ReturnType<typeof deferred<CronPreview>>>();
    const previewCron = vi.fn((expr: string) => {
      const answer = deferred<CronPreview>();
      pending.set(expr, answer);
      return answer.promise;
    });
    const { main } = await open("/automations/new", { previewCron });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const preview = () => text(find(main(), '[data-part="cron-preview"]'));
    for (const expr of ["1 * * * *", "2 * * * *", "3 * * * *"]) {
      type(control(main(), "Cron expression"), expr);
      vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS);
    }
    pending.get("3 * * * *")?.resolve({ text: "Third", next: [], problem: null });
    await settle();
    flush();
    expect(preview()).toBe("Third");
    pending.get("1 * * * *")?.resolve({ text: "First", next: [], problem: null });
    pending.get("2 * * * *")?.reject(new Error("too late"));
    await settle();
    flush();
    expect(preview()).toBe("Third");

    previewCron.mockClear();
    type(control(main(), "Cron expression"), "  ");
    vi.advanceTimersByTime(CRON_PREVIEW_DELAY_MS);
    expect(preview()).toBe("");
    expect(previewCron).not.toHaveBeenCalled();
  });

  const focused = (selector: string) => () => document.activeElement?.matches(selector) === true;
  it.each([
    ["the name", "name is already used", "schedule", focused("input[maxlength='80']")],
    ["the prompt", "unknown placeholder {{foo}}", "schedule", focused("textarea")],
    [
      "the permission",
      "ask is refused: it runs unattended",
      "schedule",
      focused("[value=accept_edits]"),
    ],
    [
      "the workflow",
      "unknown workflow nope",
      "schedule",
      focused("select:has(option[value=review])"),
    ],
    [
      "the watched repo",
      "repo web is not available",
      "issue",
      focused("select:has(option[value=web])"),
    ],
  ])(
    "shows a 400 about %s at that field and focuses it",
    async (_what, message, kind, isFocused) => {
      const { main } = await open("/automations/new", {
        listWorkflows: () => Promise.resolve([quickWorkflow, workflowInfo("review")]),
        createAutomation: () => Promise.reject(badRequest(message)),
      });
      if (kind === "issue") await click(control(main(), "New issue"));
      await click(findByText(main(), "button", "Create automation"));
      await settle();
      flush();
      expect(isFocused()).toBe(true);
      expect(text(find(main(), '[role="alert"]'))).toBe(message);
    },
  );

  it("shows a 400 about the session's repos by the repo picker, and focuses the message", async () => {
    const { main } = await open("/automations/new", {
      createAutomation: () => Promise.reject(badRequest("repo web is not available")),
    });
    await click(findByText(main(), "button", "Create automation"));
    await settle();
    flush();
    const alert = find(main(), '[role="alert"]');
    expect(text(alert)).toBe("repo web is not available");
    expect(document.activeElement).toBe(alert);
    expect(alert.closest('section[aria-label="Session to start"]')).not.toBeNull();
  });

  it("shows a refusal that is not a 400 by Save, even when it names a field", async () => {
    const { main } = await open("/automations/new", {
      createAutomation: () => Promise.reject(conflict("name is already used")),
    });
    await click(findByText(main(), "button", "Create automation"));
    await settle();
    flush();
    const alert = find(main(), '[role="alert"]');
    expect(text(alert)).toBe("name is already used");
    expect(document.activeElement).toBe(alert);
    expect(control(main(), "Name").getAttribute("aria-invalid")).toBeNull();
    expect(alert.closest('section[aria-label="Session to start"]')).toBeNull();
  });

  it("keeps the confirmation open with the reason when the delete fails", async () => {
    const { main } = await open("/automations/a1", {
      deleteAutomation: () => Promise.reject(conflict("it is running")),
    });
    await click(findByText(main(), "button", "Delete"));
    await click(findByText(main(), "button", "Delete automation"));
    await settle();
    flush();
    const confirm = find(main(), '[aria-label="Delete this automation"]');
    expect(text(find(confirm, '[role="alert"]'))).toBe("it is running");
    expect(location.pathname).toBe("/automations/a1");
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

  it.each(["", "twelve", "0", "1.5"])(
    "refuses %j as a number, saying what it needs",
    async (typed) => {
      const runAutomation = vi.fn<Api["runAutomation"]>();
      const { main } = await open("/automations/a2", { runAutomation });
      await click(findByText(main(), "button", "Run now"));
      type(control(main(), "Issue number"), typed);
      await click(findByText(main(), "button", "Run"));
      expect(runAutomation).not.toHaveBeenCalled();
      expect(text(find(main(), '[aria-label="Run now"] [role="alert"]'))).toBe(
        "Give the issue number, a whole number such as 12.",
      );
    },
  );

  it("goes back to the Run now button on Cancel", async () => {
    const { main } = await open("/automations/a2");
    await click(findByText(main(), "button", "Run now"));
    await click(findByText(main(), "button", "Cancel"));
    expect(main().querySelector('form[aria-label="Run now"]')).toBeNull();
    expect(findByText(main(), "button", "Run now")).toBeTruthy();
  });

  it("reads the history again after a run", async () => {
    const listAutomationRuns = vi.fn(() => Promise.resolve([]));
    const { main } = await open("/automations/a1", {
      listAutomationRuns,
      runAutomation: () => Promise.resolve(makeRun("r9", { session_id: null, outcome: "skipped" })),
    });
    expect(listAutomationRuns).toHaveBeenCalledTimes(1);
    await click(findByText(main(), "button", "Run now"));
    expect(listAutomationRuns).toHaveBeenCalledTimes(2);
    expect(text(find(main(), '[aria-label="Run now"] [role="status"]'))).toBe("Skipped.");
  });

  it("says why the history could not be read, and while it is being read", async () => {
    const failed = await open("/automations/a1", {
      listAutomationRuns: () => Promise.reject(new Error("Cannot reach the server")),
    });
    expect(text(find(failed.main(), '[role="alert"]'))).toBe(
      "The run history could not be loaded: Cannot reach the server",
    );
    unmountAll();
    const reading = await open("/automations/a1", {
      listAutomationRuns: () => new Promise(() => undefined),
    });
    expect(reading.main().textContent).toContain("Loading the run history…");
  });
});

describe("Automation page", () => {
  it("says why an automation could not be loaded", async () => {
    const { main } = await open("/automations/a1", {
      getAutomation: () => Promise.reject(new Error("automation not found")),
    });
    expect(text(find(main(), "h1"))).toBe("Automation");
    expect(text(find(main(), '[role="alert"]'))).toBe(
      "The automation could not be loaded: automation not found",
    );
    expect(main().querySelector("form")).toBeNull();
  });

  it("says it is loading until the automation is read", async () => {
    const { main } = await open("/automations/a1", {
      getAutomation: () => new Promise(() => undefined),
    });
    expect(main().textContent).toContain("Loading the automation…");
  });

  it("names a new automation and offers neither Run now nor a history", async () => {
    const { main } = await open("/automations/new");
    expect(text(find(main(), "h1"))).toBe("New automation");
    expect(main().querySelector('[aria-label="Run now"]')).toBeNull();
    expect(main().textContent).not.toContain("Run history");
    expect(main().textContent).not.toContain("Delete");
  });

  it("shows what fires it and why Calin switched it off", async () => {
    const { main } = await open("/automations/a3");
    expect(text(find(main(), "h1"))).toBe("Review PRs");
    expect(main().textContent).toContain("New pull request in web");
    expect(main().textContent).toContain("Switched off by Calin: 3 runs in a row failed");
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
