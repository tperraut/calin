import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { IssueQuery } from "../api/http";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Issue } from "../gen/Issue";
import type { IssueDetail } from "../gen/IssueDetail";
import type { Repo } from "../gen/Repo";
import type { RunnerLimits } from "../gen/RunnerLimits";
import { conflict, deferred, makeRepo, settle } from "../test/fakes";
import { makeIssue, makeLimits, makeWindow } from "../test/integrations";
import { click, find, findByText, mount, type, unmountAll, until } from "../test/render";
import { Composer, type IssueSource } from "./Composer";
import { ISSUE_SEARCH_DELAY_MS } from "./IssueList";

afterEach(() => {
  vi.useRealTimers();
  unmountAll();
});

const home = makeRepo("notes", { home: true });
const issues = [
  makeIssue(12, { title: "Add retry to the uploader", labels: ["bug", "agent"], author: "ada" }),
  makeIssue(15, { title: "Document the rate limiter", author: null }),
];

function setup(
  options: {
    repos?: Repo[];
    source?: Partial<IssueSource> | null;
    limits?: RunnerLimits[];
  } = {},
) {
  const list = vi.fn<(repoId: string, query: IssueQuery) => Promise<Issue[]>>(() =>
    Promise.resolve(issues),
  );
  const get = vi.fn<(repoId: string, number: number) => Promise<IssueDetail>>((_repo, number) =>
    Promise.resolve({
      issue: issues.find((one) => one.number === number) ?? makeIssue(number),
      body: "The uploader gives up after **one** failure.",
    }),
  );
  const source: IssueSource | undefined =
    options.source === null ? undefined : { list, get, ...options.source };
  const onCreate = vi.fn<(request: CreateSessionRequest) => Promise<void>>(() => Promise.resolve());
  const host = mount(() => (
    <Composer
      runners={["claude", "mock"]}
      registry={{ status: "ready", repos: options.repos ?? [home, makeRepo("api")] }}
      sets={[]}
      onCreate={onCreate}
      onRegisterRepo={() => Promise.reject(new Error("no"))}
      onRemoveRepo={() => Promise.resolve()}
      onCloneRepo={() => Promise.resolve()}
      onRetryRepos={() => {}}
      issues={source}
      limits={options.limits}
      now={2_000_000}
    />
  ));
  const disclosure = () =>
    [...host.querySelectorAll("details")].find(
      (details) => details.querySelector("summary")?.textContent?.includes("Issues") === true,
    );
  async function openIssues(): Promise<void> {
    const details = disclosure();
    if (!details) throw new Error("no Issues disclosure");
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    flush();
    // The list's code arrives when the disclosure first opens.
    await until(() => host.querySelector('input[type="search"]') !== null);
  }
  const prompt = () => find<HTMLTextAreaElement>(host, "textarea");
  async function start(text = ""): Promise<CreateSessionRequest | undefined> {
    type(prompt(), text);
    await click(findByText(host, "button", "Start"));
    return onCreate.mock.lastCall?.[0];
  }
  return { host, list, get, onCreate, disclosure, openIssues, prompt, start };
}

describe("Composer: starting from an issue", () => {
  it("offers Issues for the primary repo only when GitHub is usable and a repo is selected", () => {
    expect(setup().disclosure()?.textContent).toContain("Issuesnotes");
    unmountAll();
    expect(setup({ source: null }).disclosure()).toBeUndefined();
    unmountAll();
    // No home repo and nothing remembered: nothing is selected, so there is no primary repo.
    expect(setup({ repos: [makeRepo("api")] }).disclosure()).toBeUndefined();
  });

  it("lists the open issues of the primary repo with labels, author and age", async () => {
    const { host, list, openIssues } = await Promise.resolve(setup());
    await openIssues();
    expect(list).toHaveBeenCalledWith("notes", { state: "open" });
    const rows = [...host.querySelectorAll('ul[aria-label="Issues of notes"] > li')].map(
      (li) => li.textContent,
    );
    expect(rows[0]).toContain("#12Add retry to the uploader");
    expect(rows[0]).toContain("bug");
    expect(rows[0]).toContain("agent");
    expect(rows[0]).toContain("ada");
    expect(rows[0]).toContain("updated ");
    expect(rows[1]).toContain("#15Document the rate limiter");
  });

  it("sends the search once typing pauses, and the state when it changes", async () => {
    const { host, list, openIssues } = setup();
    await openIssues();
    list.mockClear();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const search = find<HTMLInputElement>(host, 'input[type="search"]');
    type(search, "ret");
    vi.advanceTimersByTime(100);
    type(search, "retry & co");
    vi.advanceTimersByTime(ISSUE_SEARCH_DELAY_MS - 1);
    expect(list).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(list.mock.calls).toEqual([["notes", { state: "open", q: "retry & co" }]]);
    vi.useRealTimers();

    await click(findByText(host, "button", "Closed"));
    await settle();
    expect(list.mock.lastCall).toEqual(["notes", { state: "closed", q: "retry & co" }]);
    expect(find(host, '[aria-label="Issue state"] [aria-pressed="true"]').textContent).toBe(
      "Closed",
    );
  });

  it("shows the server's reason once and no list when the repo's issues cannot be listed", async () => {
    const { host, openIssues } = setup({
      source: { list: () => Promise.reject(conflict("notes has no GitHub remote")) },
    });
    await openIssues();
    // Waits for the search box that never comes; the reason is what shows.
    const status = [...host.querySelectorAll('[role="status"]')].filter((one) =>
      one.textContent?.includes("notes has no GitHub remote"),
    );
    expect(status).toHaveLength(1);
    expect(host.querySelector('input[type="search"]')).toBeNull();
    expect(host.querySelector('ul[aria-label="Issues of notes"]')).toBeNull();
  });

  it("says when the issues could not be listed, and lists them on Retry", async () => {
    const list = vi
      .fn<(repoId: string, query: IssueQuery) => Promise<Issue[]>>()
      .mockRejectedValueOnce(new Error("Cannot reach the server"))
      .mockResolvedValue(issues);
    const { host, openIssues } = setup({ source: { list } });
    await openIssues();
    await settle();
    flush();
    expect(find(host, '[role="alert"]').textContent).toContain(
      "The issues could not be loaded: Cannot reach the server",
    );
    expect(host.querySelector('ul[aria-label="Issues of notes"]')).toBeNull();

    await click(findByText(find(host, '[role="alert"]'), "button", "Retry"));
    expect(list).toHaveBeenCalledTimes(2);
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(host.querySelectorAll('ul[aria-label="Issues of notes"] > li')).toHaveLength(2);
  });

  it("says when there is no issue, or none matches the search", async () => {
    const { host, openIssues } = setup({ source: { list: () => Promise.resolve([]) } });
    await openIssues();
    await settle();
    flush();
    expect(host.textContent).toContain("No open issues in notes.");

    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    type(find<HTMLInputElement>(host, 'input[type="search"]'), " flaky ");
    vi.advanceTimersByTime(ISSUE_SEARCH_DELAY_MS);
    vi.useRealTimers();
    await settle();
    flush();
    expect(host.textContent).toContain('No open issue matches "flaky".');
  });

  it("keeps the list it has while a new one loads, and drops an answer a newer request overtook", async () => {
    const first = deferred<Issue[]>();
    const closed = [makeIssue(3, { title: "Old crash", state: "closed" })];
    const list = vi
      .fn<(repoId: string, query: IssueQuery) => Promise<Issue[]>>()
      .mockResolvedValueOnce(issues)
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(issues)
      .mockResolvedValueOnce(closed);
    const { host, openIssues } = setup({ source: { list } });
    await openIssues();
    await settle();
    flush();
    const titles = () =>
      [...host.querySelectorAll('ul[aria-label="Issues of notes"] > li')].map(
        (li) => li.textContent,
      );
    expect(titles()).toHaveLength(2);

    await click(findByText(host, "button", "Closed"));
    // The open list stays on screen while the closed one is asked for.
    expect(host.textContent).not.toContain("Loading issues…");
    expect(titles()).toHaveLength(2);

    await click(findByText(host, "button", "Open"));
    await click(findByText(host, "button", "Closed"));
    expect(titles()).toEqual([expect.stringContaining("#3Old crash")]);
    first.resolve(issues);
    await settle();
    flush();
    expect(titles()).toEqual([expect.stringContaining("#3Old crash")]);
  });

  it("does not ask again for the state already shown", async () => {
    const { host, list, openIssues } = setup();
    await openIssues();
    list.mockClear();
    await click(findByText(host, "button", "Open"));
    await settle();
    expect(list).not.toHaveBeenCalled();
  });

  it("says when the picked issue's text could not be read, or that it has none", async () => {
    const failing = setup({ source: { get: () => Promise.reject(new Error("rate limited")) } });
    await failing.openIssues();
    await click(findByText(failing.host, "button", "Add retry to the uploader"));
    await settle();
    flush();
    expect(failing.host.textContent).toContain("The issue could not be loaded: rate limited");
    unmountAll();

    const pending = deferred<IssueDetail>();
    const empty = setup({ source: { get: () => pending.promise } });
    await empty.openIssues();
    await click(findByText(empty.host, "button", "Add retry to the uploader"));
    expect(empty.host.textContent).toContain("Loading the issue…");
    pending.resolve({ issue: issues[0], body: "  \n" });
    await settle();
    flush();
    expect(empty.host.textContent).toContain("The issue has no description.");
  });

  it("drops the text of an issue that was replaced before it arrived", async () => {
    const bodies = new Map([
      [12, deferred<IssueDetail>()],
      [15, deferred<IssueDetail>()],
    ]);
    const { host, openIssues } = setup({
      source: {
        get: (_repo, number) => bodies.get(number)?.promise ?? Promise.reject(new Error("unknown")),
      },
    });
    await openIssues();
    await click(findByText(host, "button", "Add retry to the uploader"));
    await openIssues();
    await click(findByText(host, "button", "Document the rate limiter"));
    bodies.get(12)?.resolve({ issue: issues[0], body: "Text of twelve" });
    await settle();
    flush();
    expect(host.textContent).not.toContain("Text of twelve");
    expect(host.textContent).toContain("Loading the issue…");
    bodies.get(15)?.resolve({ issue: issues[1], body: "Text of fifteen" });
    await settle();
    flush();
    expect(host.textContent).toContain("Text of fifteen");
  });

  it("keeps the default placeholder until an issue is picked", () => {
    const { prompt } = setup();
    expect(prompt().placeholder).toBe("Describe what the agent should do");
  });

  it("shows a picked issue as a removable chip with its body, and sends it", async () => {
    const { host, get, openIssues, prompt, start } = setup();
    await openIssues();
    await click(findByText(host, "button", "Add retry to the uploader"));
    await settle();
    flush();
    expect(get).toHaveBeenCalledWith("notes", 12);
    const chip = find<HTMLAnchorElement>(host, '[data-part="issue-chip"]');
    expect(chip.textContent).toBe("#12Add retry to the uploader");
    expect(chip.getAttribute("rel")).toBe("noopener noreferrer");
    expect(prompt().placeholder).toBe("Extra instructions (optional)");
    const body = [...host.querySelectorAll("details")].find((one) =>
      one.querySelector("summary")?.textContent?.includes("Show issue"),
    );
    expect(body?.open).toBe(false);
    expect(body?.querySelector("strong")?.textContent).toBe("one");

    // The prompt may stay empty: the issue is the task.
    expect(await start("")).toMatchObject({ prompt: "", issue: { repo_id: "notes", number: 12 } });
    // Started: the next session starts from scratch.
    expect(host.querySelector('[data-part="issue-chip"]')).toBeNull();
    expect(prompt().placeholder).toBe("Describe what the agent should do");
  });

  it("sends what is typed as extra instructions, and nothing about an issue once it is removed", async () => {
    const { host, openIssues, start, onCreate } = setup();
    await openIssues();
    await click(findByText(host, "button", "Document the rate limiter"));
    expect(await start("Keep it short")).toMatchObject({
      prompt: "Keep it short",
      issue: { repo_id: "notes", number: 15 },
    });

    await openIssues();
    await click(findByText(host, "button", "Document the rate limiter"));
    await click(find(host, 'button[aria-label="Remove issue #15"]'));
    expect(host.querySelector('[data-part="issue-chip"]')).toBeNull();
    onCreate.mockClear();
    expect(await start("")).toBeUndefined();
    const request = await start("Plain task");
    expect(request).toEqual({
      prompt: "Plain task",
      permission_mode: "ask",
      repos: [{ repo_id: "notes", mode: "live" }],
      runner: "claude",
    });
  });
});

describe("Composer: plan limits", () => {
  it("says calmly that the selected runner is refusing requests, and still starts", async () => {
    const resetsAt = 2_000_000 + 3_600_000;
    const { host, start } = setup({
      source: null,
      limits: [
        makeLimits("claude", [makeWindow("five_hour", { refused: true, resets_at: resetsAt })]),
      ],
    });
    const clock = new Date(resetsAt).toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      hourCycle: "h23",
    });
    expect(host.textContent).toContain(
      `Requests are being refused until ${clock}. A session started now will fail or wait.`,
    );
    expect(await start("Go")).toMatchObject({ prompt: "Go" });
  });

  it("says nothing for another runner's refusal", () => {
    const { host } = setup({
      source: null,
      limits: [makeLimits("mock", [makeWindow("five_hour", { refused: true })])],
    });
    expect(host.textContent).not.toContain("Requests are being refused");
  });
});
