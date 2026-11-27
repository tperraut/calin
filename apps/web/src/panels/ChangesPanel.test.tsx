import { flush } from "solid-js";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { ChangedFile } from "../gen/ChangedFile";
import type { FileDiff } from "../gen/FileDiff";
import type { RepoChanges } from "../gen/RepoChanges";
import { conflict, deferred, makePullRequest, makeSessionRepo, settle } from "../test/fakes";
import { diffs, editors, resetFakeEditors } from "../test/fakeEditor";
import {
  click,
  find,
  findByText,
  mount,
  PHONE,
  setViewportWidth,
  unmountAll,
} from "../test/render";
import { act, openWorkspace, type Workspace } from "../test/workspace";
import ChangesPanel from "./ChangesPanel";
import { loadEditorModule } from "./editor/load";

vi.mock("./editor/codemirror", async () => ({
  default: (await import("../test/fakeEditor")).fakeEditorModule,
}));

const file = (path: string, patch: Partial<ChangedFile> = {}): ChangedFile => ({
  path,
  old_path: null,
  status: "modified",
  added: 1,
  removed: 1,
  binary: false,
  ...patch,
});

const repo = (id: string, files: ChangedFile[], patch: Partial<RepoChanges> = {}): RepoChanges => ({
  session_repo_id: id,
  name: id,
  branch: null,
  base_ref: null,
  files,
  added: files.reduce((sum, one) => sum + one.added, 0),
  removed: files.reduce((sum, one) => sum + one.removed, 0),
  ...patch,
});

const diffOf = (path: string, patch: Partial<FileDiff> = {}): FileDiff => ({
  path,
  old_path: null,
  old: "before\n",
  new: "after\n",
  binary: false,
  too_large: false,
  ...patch,
});

const api = [makeSessionRepo("api", { primary: true, branch: "calin/s1" })];

async function open(routes: Partial<Api>, repos = api): Promise<Workspace & { host: HTMLElement }> {
  const workspace = await openWorkspace({ routes, repos });
  const host = mount(() => (
    <ChangesPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
  ));
  await act();
  return { ...workspace, host };
}

const groups = (host: HTMLElement): HTMLElement[] => [
  ...find(host, 'section[aria-label="Changed files"]').querySelectorAll<HTMLElement>("section"),
];
const fileButton = (host: HTMLElement, path: string): HTMLButtonElement => {
  for (const button of host.querySelectorAll<HTMLButtonElement>("li > button")) {
    if (button.textContent.includes(path)) return button;
  }
  throw new Error(`no file row for ${path}`);
};

// The first import of the editor module takes a few turns; later ones are immediate.
beforeAll(() => loadEditorModule());
beforeEach(resetFakeEditors);
afterEach(() => {
  vi.useRealTimers();
  unmountAll();
});

describe("ChangesPanel", () => {
  it("says so while loading, then when there is nothing", async () => {
    const pending = deferred<RepoChanges[]>();
    const { host } = await open({ getChanges: () => pending.promise });
    expect(host.textContent).toContain("Loading changes…");
    pending.resolve([repo("api", [])]);
    await act();
    expect(host.textContent).not.toContain("Loading changes…");
    expect(host.textContent).toContain("No changes yet");
  });

  it("shows the server's message when the changes cannot be loaded, and tries again", async () => {
    const getChanges = vi
      .fn<Api["getChanges"]>()
      .mockRejectedValueOnce(conflict("the worktree is being prepared"))
      .mockResolvedValue([repo("api", [file("a.txt")])]);
    const { host } = await open({ getChanges });
    expect(find(host, '[role="alert"]').textContent).toContain("the worktree is being prepared");

    await click(findByText(host, "button", "Try again"));
    await act();
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(host.textContent).toContain("a.txt");
  });

  it("shows one repo: name, branch, totals, and each file with its status in words", async () => {
    const { host } = await open({
      getChanges: () =>
        Promise.resolve([
          repo(
            "api",
            [
              file("src/health.rs", { status: "added", added: 12, removed: 0 }),
              file("src/main.rs", { added: 3, removed: 1 }),
              file("src/old.rs", { status: "deleted", added: 0, removed: 20 }),
              file("src/new_name.rs", { status: "renamed", old_path: "src/old_name.rs" }),
              file("notes.txt", { status: "untracked", added: 2, removed: 0 }),
              file("logo.png", { status: "untracked", added: 0, removed: 0, binary: true }),
            ],
            { branch: "calin/s1", added: 18, removed: 22 },
          ),
        ]),
    });

    // One repo: the count needs no "in n repos".
    expect(find(host, '[role="status"]').textContent).toContain("6 files changed");
    expect(find(host, '[role="status"]').textContent).not.toContain("repos");
    expect(find(host, '[role="status"]').textContent).toContain("18 added, 22 removed");

    const [group] = groups(host);
    const header = find<HTMLButtonElement>(group, "h3 button");
    expect(header.textContent).toContain("api");
    expect(header.textContent).toContain("calin/s1");
    expect(header.textContent).toContain("+18");
    expect(header.textContent).toContain("−22");
    expect(header.textContent).toContain("6 files");
    expect(header.getAttribute("aria-expanded")).toBe("true");

    const rows = [...group.querySelectorAll("li")].map((row) => row.textContent);
    expect(rows).toHaveLength(6);
    expect(rows[0]).toContain("Aadded");
    expect(rows[0]).toContain("src/health.rs");
    expect(rows[0]).toContain("12 added, 0 removed");
    expect(rows[1]).toContain("Mmodified");
    expect(rows[2]).toContain("Ddeleted");
    expect(rows[3]).toContain("Rrenamed");
    expect(rows[3]).toContain("src/old_name.rs → src/new_name.rs");
    expect(rows[4]).toContain("Uuntracked");
    expect(rows[5]).toContain("binary");
    // A binary file has no line counts to show.
    expect(rows[5]).not.toContain("added,");

    await click(header);
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(find(group, "ul").hidden).toBe(true);
  });

  it("groups several repos, primary first, with a session total and quiet repos on one line", async () => {
    const repos = [
      makeSessionRepo("web", { primary: true }),
      makeSessionRepo("api"),
      makeSessionRepo("docs"),
      makeSessionRepo("infra"),
      makeSessionRepo("cli"),
    ];
    const { host } = await open(
      {
        // The server's order is not the session's.
        getChanges: () =>
          Promise.resolve([
            repo("api", [
              file("a.rs", { added: 10, removed: 2 }),
              file("b.rs", { added: 5, removed: 0 }),
            ]),
            repo("cli", []),
            repo("docs", [file("guide.md", { added: 1, removed: 1 })]),
            repo("infra", []),
            repo("web", [file("App.tsx", { added: 4, removed: 4 })]),
          ]),
      },
      repos,
    );

    const total = find(host, '[role="status"]').textContent;
    expect(total).toContain("4 files changed in 3 of 5 repos");
    expect(total).toContain("20 added, 7 removed");

    const all = groups(host);
    expect(all.map((group) => group.getAttribute("aria-label"))).toEqual([
      "web",
      "api",
      "docs",
      "infra",
      "cli",
    ]);
    expect(find(all[1], "h3 button").textContent).toContain("2 files");
    expect(find(all[1], "h3 button").textContent).toContain("+15");
    expect(all[1].querySelectorAll("li")).toHaveLength(2);
    // Nothing to open or expand for a repo without changes.
    expect(all[3].textContent).toBe("infraNo changes");
    expect(all[3].querySelector("button")).toBeNull();
    expect(all[4].textContent).toBe("cliNo changes");
  });

  it("gives the repo name its room and lets a long branch name give way", async () => {
    const branch = "calin/add-a-health-check-endpoint-and-cover-it-with-a-test-and-update-the-docs";
    const { host } = await open(
      {
        getChanges: () =>
          Promise.resolve([
            repo("api", [file("a.txt")], { branch }),
            repo("design-system", [], { branch }),
          ]),
      },
      [makeSessionRepo("api", { primary: true, branch }), makeSessionRepo("design-system")],
    );
    for (const [group, name] of [
      [groups(host)[0], "api"],
      [groups(host)[1], "design-system"],
    ] as const) {
      const repoName = find(group, '[data-part="repo"]');
      const branchName = find(group, '[data-part="branch"]');
      // The repo name never shrinks for the branch: it is cut only past its own share of the row.
      expect(repoName.textContent).toBe(name);
      expect(repoName.classList.contains("shrink-0")).toBe(true);
      expect(repoName.classList.contains("min-w-0")).toBe(false);
      // The branch takes what is left, and is the one that is cut short.
      expect(branchName.classList.contains("min-w-0")).toBe(true);
      expect(branchName.classList.contains("truncate")).toBe(true);
      expect(branchName.classList.contains("shrink-0")).toBe(false);
      // Cut on screen only: the whole name is its text, for a screen reader, and its tooltip.
      expect(branchName.textContent).toBe(branch);
      expect(branchName.title).toBe(branch);
      // Both sit in the same row, the name first.
      expect(repoName.parentElement).toBe(branchName.parentElement);
      expect(repoName.nextElementSibling).toBe(branchName);
    }
    // With the keyboard on the row, the branch shows in full.
    const header = find(groups(host)[0], "h3 button");
    expect(header.classList.contains("group")).toBe(true);
    expect(find(header, '[data-part="branch"]').className).toContain(
      "group-focus-visible:whitespace-normal",
    );
    // The totals keep their place at the end of the row.
    expect(header.lastElementChild?.classList.contains("shrink-0")).toBe(true);
  });

  it("has no branch element for a repo without a session branch", async () => {
    const { host } = await open({
      getChanges: () => Promise.resolve([repo("api", [file("a.txt")])]),
    });
    expect(groups(host)[0].querySelector('[data-part="branch"]')).toBeNull();
    expect(find(groups(host)[0], '[data-part="repo"]').textContent).toBe("api");
  });

  it("shows the list the session already holds while it asks for a fresh one", async () => {
    const fresh = deferred<RepoChanges[]>();
    const getChanges = vi
      .fn<Api["getChanges"]>()
      .mockResolvedValueOnce([repo("api", [file("old.txt")])])
      .mockReturnValueOnce(fresh.promise);
    const workspace = await openWorkspace({ routes: { getChanges }, repos: api });
    // Read before the panel exists, as the session screen does for the tab's count.
    workspace.store.changes.now();
    await act();
    const host = mount(() => (
      <ChangesPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
    ));
    flush();
    // No "Loading changes…": what is known shows at once, and opening asks again.
    expect(host.textContent).toContain("old.txt");
    expect(host.textContent).not.toContain("Loading changes…");
    expect(getChanges).toHaveBeenCalledTimes(2);

    fresh.resolve([repo("api", [file("new.txt")])]);
    await act();
    expect(host.textContent).toContain("new.txt");
    expect(host.textContent).not.toContain("old.txt");
  });

  it("refreshes, debounced, when a file changes, and at once on the refresh button", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue([repo("api", [])]);
    const { host, emit } = await open({ getChanges });
    expect(getChanges).toHaveBeenCalledTimes(1);

    vi.useFakeTimers();
    getChanges.mockResolvedValue([repo("api", [file("src/main.rs")])]);
    emit({ type: "file_changed", session_repo_id: "api", path: "src/main.rs", op: "edit" });
    vi.advanceTimersByTime(300);
    emit({ type: "file_changed", session_repo_id: "api", path: "src/lib.rs", op: "edit" });
    vi.advanceTimersByTime(499);
    expect(getChanges).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1);
    expect(getChanges).toHaveBeenCalledTimes(2);
    vi.useRealTimers();
    await act();
    expect(host.textContent).toContain("src/main.rs");

    await click(find(host, 'button[aria-label="Refresh the changes"]'));
    expect(getChanges).toHaveBeenCalledTimes(3);
  });

  it("refreshes when the session's status changes", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue([repo("api", [])]);
    const { emit, caughtUp } = await open({ getChanges });
    emit({ type: "session_status", status: "running", reason: null });
    caughtUp();
    vi.useFakeTimers();
    vi.advanceTimersByTime(600);
    const before = getChanges.mock.calls.length;

    emit({ type: "session_status", status: "review", reason: null });
    vi.advanceTimersByTime(500);
    expect(getChanges).toHaveBeenCalledTimes(before + 1);
  });

  it("does not refresh behind another panel or in a background tab, but does on the way back", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue([repo("api", [])]);
    const { emit, setActive } = await open({ getChanges });
    vi.useFakeTimers();

    setActive(false);
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    emit({ type: "file_changed", session_repo_id: "api", path: "b.rs", op: "edit" });
    vi.advanceTimersByTime(5000);
    expect(getChanges).toHaveBeenCalledTimes(1);
    setActive(true);
    expect(getChanges).toHaveBeenCalledTimes(2);
    await settle();
    // Nothing happened since: coming back again fetches nothing.
    setActive(false);
    setActive(true);
    expect(getChanges).toHaveBeenCalledTimes(2);

    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    emit({ type: "file_changed", session_repo_id: "api", path: "c.rs", op: "edit" });
    vi.advanceTimersByTime(5000);
    expect(getChanges).toHaveBeenCalledTimes(2);
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(getChanges).toHaveBeenCalledTimes(3);
  });

  it("shows the diff of the selected file, and reloads it when the changes refresh", async () => {
    const getDiff = vi.fn<Api["getDiff"]>().mockResolvedValue(diffOf("src/main.rs"));
    const { host, emit } = await open({
      getChanges: () => Promise.resolve([repo("api", [file("src/main.rs")])]),
      getDiff,
    });
    await click(fileButton(host, "src/main.rs"));
    await act();

    expect(getDiff).toHaveBeenCalledWith("s1", "api", "src/main.rs");
    expect(fileButton(host, "src/main.rs").getAttribute("aria-current")).toBe("true");
    expect(diffs.filter((one) => !one.destroyed)).toHaveLength(1);
    expect(diffs[0].options).toMatchObject({
      path: "src/main.rs",
      original: "before\n",
      modified: "after\n",
      layout: "split",
    });
    expect(find<HTMLButtonElement>(host, 'button[aria-label="Next change"]').disabled).toBe(false);

    // The same content again leaves the view alone; new content replaces it.
    vi.useFakeTimers();
    emit({ type: "file_changed", session_repo_id: "api", path: "src/main.rs", op: "edit" });
    vi.advanceTimersByTime(500);
    vi.useRealTimers();
    await act();
    expect(getDiff).toHaveBeenCalledTimes(2);
    expect(diffs).toHaveLength(1);

    getDiff.mockResolvedValue(diffOf("src/main.rs", { new: "after, again\n" }));
    vi.useFakeTimers();
    emit({ type: "file_changed", session_repo_id: "api", path: "src/main.rs", op: "edit" });
    vi.advanceTimersByTime(500);
    vi.useRealTimers();
    await act();
    expect(diffs).toHaveLength(2);
    expect(diffs[0].destroyed).toBe(true);
    expect(diffs[1].options.modified).toBe("after, again\n");
  });

  it("offers side by side on a wide screen, with a toggle, and inline on a phone", async () => {
    const routes: Partial<Api> = {
      getChanges: () => Promise.resolve([repo("api", [file("a.txt")])]),
      getDiff: () => Promise.resolve(diffOf("a.txt")),
    };
    const wide = await open(routes);
    await click(fileButton(wide.host, "a.txt"));
    await act();
    const toggle = findByText<HTMLButtonElement>(wide.host, "button", "Side by side");
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    expect(diffs.at(-1)?.options.layout).toBe("split");
    await click(toggle);
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    expect(diffs.at(-1)?.options.layout).toBe("unified");
    unmountAll();

    setViewportWidth(PHONE);
    resetFakeEditors();
    const phone = await open(routes);
    await click(fileButton(phone.host, "a.txt"));
    await act();
    expect(diffs.at(-1)?.options.layout).toBe("unified");
    expect(phone.host.textContent).not.toContain("Side by side");
    // Two levels: the list steps aside for the diff, and comes back.
    const list = find(phone.host, 'section[aria-label="Changed files"]');
    expect(list.hidden).toBe(true);
    await click(findByText(phone.host, "button", "Changes"));
    expect(list.hidden).toBe(false);
    expect(phone.host.querySelector('section[aria-label="Diff"]')).toBeNull();
  });

  it("shows a notice instead of a diff for a binary file and for one that is too large", async () => {
    const { host } = await open({
      getChanges: () =>
        Promise.resolve([
          repo("api", [file("logo.png", { binary: true }), file("dump.sql", { added: 90000 })]),
        ]),
      getDiff: (_id, _repo, path) =>
        Promise.resolve(
          path === "logo.png"
            ? diffOf(path, { old: null, new: null, binary: true })
            : diffOf(path, { old: "small\n", new: null, too_large: true }),
        ),
    });

    await click(fileButton(host, "logo.png"));
    await act();
    const pane = find(host, 'section[aria-label="Diff"]');
    expect(pane.textContent).toContain("Binary file: there is no text to compare.");
    expect(diffs).toHaveLength(0);
    expect(pane.querySelector('button[aria-label="Next change"]')).toBeNull();

    await click(fileButton(host, "dump.sql"));
    await act();
    expect(pane.textContent).toContain("This file is too large to show as a diff.");
    expect(pane.textContent).not.toContain("Binary file");
    expect(diffs).toHaveLength(0);
    expect(editors).toHaveLength(0);
  });

  it("shows the one side that exists for an added and for a deleted file", async () => {
    const { host } = await open({
      getChanges: () =>
        Promise.resolve([
          repo("api", [file("new.rs", { status: "added" }), file("old.rs", { status: "deleted" })]),
        ]),
      getDiff: (_id, _repo, path) =>
        Promise.resolve(
          path === "new.rs"
            ? diffOf(path, { old: null, new: "fn new() {}\n" })
            : diffOf(path, { old: "fn old() {}\n", new: null }),
        ),
    });

    await click(fileButton(host, "new.rs"));
    await act();
    expect(host.textContent).toContain("New file: all of it was added.");
    expect(editors.at(-1)?.options).toMatchObject({ text: "fn new() {}\n", readOnly: true });
    expect(diffs).toHaveLength(0);

    await click(fileButton(host, "old.rs"));
    await act();
    expect(host.textContent).toContain("Deleted file: this is what it contained.");
    expect(editors.at(-1)?.options).toMatchObject({ text: "fn old() {}\n", readOnly: true });
    // There is nothing on disk to open.
    expect(findByText<HTMLButtonElement>(host, "button", "Open in Files").disabled).toBe(true);
  });

  it("says so when the selected file is no longer among the changes, instead of a stale diff", async () => {
    const getChanges = vi
      .fn<Api["getChanges"]>()
      .mockResolvedValue([repo("api", [file("a.txt"), file("b.txt")])]);
    const getDiff = vi.fn<Api["getDiff"]>().mockResolvedValue(diffOf("a.txt"));
    const { host } = await open({ getChanges, getDiff });
    await click(fileButton(host, "a.txt"));
    await act();
    expect(diffs.filter((one) => !one.destroyed)).toHaveLength(1);

    getChanges.mockResolvedValue([repo("api", [file("b.txt")])]);
    await click(find(host, 'button[aria-label="Refresh the changes"]'));
    await act();

    const pane = find(host, 'section[aria-label="Diff"]');
    expect(pane.textContent).toContain("a.txt is no longer among the changes.");
    expect(diffs.filter((one) => !one.destroyed)).toHaveLength(0);
    // No diff is asked for a file that is not there anymore.
    expect(getDiff).toHaveBeenCalledTimes(1);

    await click(findByText(pane, "button", "Back to the changes"));
    expect(host.querySelector('section[aria-label="Diff"]')).toBeNull();
  });

  it("shows the server's message when a diff cannot be loaded", async () => {
    const { host } = await open({
      getChanges: () => Promise.resolve([repo("api", [file("a.txt")])]),
      getDiff: () => Promise.reject(conflict("a.txt is not among the changes")),
    });
    await click(fileButton(host, "a.txt"));
    await act();
    expect(find(host, 'section[aria-label="Diff"] [role="alert"]').textContent).toContain(
      "a.txt is not among the changes",
    );
  });

  it("hands a file over to the Files panel", async () => {
    const { host, opened } = await open({
      getChanges: () => Promise.resolve([repo("api", [file("src/a b#.rs")])]),
      getDiff: () => Promise.resolve(diffOf("src/a b#.rs")),
    });
    await click(fileButton(host, "src/a b#.rs"));
    await act();
    await click(findByText(host, "button", "Open in Files"));
    expect(opened).toEqual([{ sessionRepoId: "api", path: "src/a b#.rs" }]);
    flush();
  });
});

describe("ChangesPanel: delivery", () => {
  const set = [
    makeSessionRepo("api", { primary: true, branch: "calin/s1" }),
    makeSessionRepo("web", { branch: "calin/s1" }),
    makeSessionRepo("docs", { mode: "read" }),
  ];
  const listing = () =>
    Promise.resolve([repo("api", [file("a.rs")]), repo("web", []), repo("docs", [file("d.md")])]);

  async function shipped(
    status: "review" | "shipping",
    pullRequests = [makePullRequest("web", 7)],
  ) {
    const workspace = await openWorkspace({
      routes: { getChanges: listing },
      repos: set,
      status,
      pullRequests,
    });
    const host = mount(() => (
      <ChangesPanel store={workspace.store} active={workspace.active()} links={workspace.links} />
    ));
    await act();
    return { ...workspace, host };
  }

  it("shows no pull request section, and no link, before any exists", async () => {
    const { host } = await shipped("review", []);
    expect(host.querySelector('[role="region"][aria-label="Pull requests"]')).toBeNull();
    expect(host.querySelector("[data-pull-request]")).toBeNull();
  });

  it("puts the pull requests above the file groups, and links each repo group to its own", async () => {
    const { host, emit, caughtUp } = await shipped("shipping");
    caughtUp();
    emit({ type: "pull_request_opened", pull_request: makePullRequest("api", 12) });

    const list = find(host, 'section[aria-label="Changed files"]');
    const section = find(list, '[role="region"][aria-label="Pull requests"]');
    const first = find(list, 'section[aria-label="api"]');
    expect(section.compareDocumentPosition(first) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(find(section, '[data-part="roll-up"]').textContent).toBe("2 pull requests");

    const ofApi = find<HTMLAnchorElement>(first, "h3 a[data-pull-request]");
    expect([ofApi.textContent, ofApi.getAttribute("href")]).toEqual([
      "#12",
      "https://github.com/acme/api/pull/12",
    ]);
    // Beside the toggle, not inside it: a link in a button could not be followed.
    expect(find(first, "h3 button").querySelector("a")).toBeNull();
    // A repo with nothing changed keeps its one line, and its link.
    const ofWeb = find<HTMLAnchorElement>(
      find(list, 'section[aria-label="web"]'),
      "a[data-pull-request]",
    );
    expect(ofWeb.getAttribute("href")).toBe("https://github.com/acme/web/pull/7");
    expect(
      find(list, 'section[aria-label="docs"]').querySelector("[data-pull-request]"),
    ).toBeNull();
  });

  it("offers to open pull requests under the summary, and asks the screen for the form", async () => {
    const { host, shipForms } = await shipped("review", []);
    const action = find<HTMLButtonElement>(host, '[aria-label="Delivery"] button');
    expect(action.textContent).toBe("Open pull requests");
    await click(action);
    expect(shipForms()).toBe(1);
  });

  it("hands the message for the agent over to the screen", async () => {
    const { host, drafts } = await shipped("shipping", [
      makePullRequest("web", 7, { checks: "failing" }),
    ]);
    await click(findByText(host, "button", "Send back to the agent"));
    expect(drafts).toHaveLength(1);
    expect(drafts[0]).toContain("- web: checks are failing on https://github.com/acme/web/pull/7");
  });

  it("leaves the action to the header on a phone", async () => {
    setViewportWidth(PHONE);
    const { host } = await shipped("review", []);
    expect(host.querySelector('[aria-label="Delivery"]')).toBeNull();
  });
});
