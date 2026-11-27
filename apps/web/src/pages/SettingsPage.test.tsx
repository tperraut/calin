import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { HttpError, type Api } from "../api/http";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { Settings } from "../gen/Settings";
import { AppContext, createAppState } from "../state/app";
import {
  badRequest,
  conflict,
  deferred,
  fakeApi,
  FakeSocket,
  immediately,
  makeMissingRepo,
  makeRepo,
  settle,
  unreachable,
} from "../test/fakes";
import { click, control, find, findByText, mount, type, unmountAll } from "../test/render";
import { quickWorkflow } from "../test/fakes";
import { workflowInfo } from "../test/workflows";
import SettingsPage from "./SettingsPage";

afterEach(unmountAll);

const unset: Settings = { home_repo: null, scan_roots: [], clone_root: null, max_parallel: null };
const stored: Settings = {
  home_repo: "/home/me/code/notes",
  scan_roots: ["/home/me/code", "/srv/git"],
  clone_root: null,
  max_parallel: null,
};
const home = makeRepo("notes", { home: true, path: "/home/me/code/notes" });
const catalog: Repo[] = [
  home,
  makeRepo("api", { declared: true }),
  makeRepo("docs", { declared: true }),
  makeMissingRepo("web"),
  makeRepo("scratch"),
];
const sets: RepoSet[] = [
  { name: "storefront", repo_ids: ["api", "web"] },
  { name: "solo", repo_ids: ["docs"] },
];

async function open(routes: Partial<Api> = {}) {
  const api = fakeApi({
    getSettings: () => Promise.resolve(stored),
    listRepos: () => Promise.resolve(catalog),
    listRepoSets: () => Promise.resolve(sets),
    ...routes,
  });
  const state = createAppState({ api, socket: new FakeSocket(), schedule: immediately });
  const host = mount(() => (
    <AppContext value={state}>
      <SettingsPage />
    </AppContext>
  ));
  await settle();
  flush();
  const form = () => find<HTMLFormElement>(host, 'form[aria-label="Settings"]');
  const roots = () =>
    [...host.querySelectorAll('ul[aria-label="Scan roots"] > li > span')].map(
      (root) => root.textContent,
    );
  const save = () => click(findByText(form(), "button", "Save"));
  /** The alert shown with the control labelled `label`, if any. */
  const errorAt = (label: string): string | undefined => {
    const described = control(form(), label).getAttribute("aria-describedby");
    return described === null ? undefined : document.getElementById(described)?.textContent;
  };
  const summary = () => find(host, "section section");
  return { host, state, form, roots, save, errorAt, summary };
}

describe("SettingsPage", () => {
  it("says it is loading, then shows what the server stores", async () => {
    const pending = deferred<Settings>();
    const { host, form, roots } = await open({ getSettings: () => pending.promise });
    expect(host.textContent).toContain("Loading settings…");
    expect(host.querySelector("form")).toBeNull();

    pending.resolve(stored);
    await settle();
    flush();
    expect(host.textContent).not.toContain("Loading settings…");
    expect(control(form(), "Home repo").value).toBe("/home/me/code/notes");
    expect(roots()).toEqual(["/home/me/code", "/srv/git"]);
    // Not set: the field is empty and says what applies instead.
    const cloneRoot = control(form(), "Clone root");
    expect(cloneRoot.value).toBe("");
    expect(cloneRoot.placeholder).toBe("Server default: <data dir>/repos");
    expect(find(host, 'a[href="/"]').getAttribute("aria-label")).toBe("Back to sessions");
  });

  it("shows the clone root the server reports", async () => {
    const { form } = await open({
      getSettings: () =>
        Promise.resolve({ ...stored, clone_root: "/data/calin/repos", max_parallel: null }),
    });
    expect(control(form(), "Clone root").value).toBe("/data/calin/repos");
  });

  it("shows why the settings could not be loaded, and tries again", async () => {
    const getSettings = vi
      .fn<Api["getSettings"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(stored);
    const { host, form } = await open({ getSettings });
    expect(find(host, '[role="alert"]').textContent).toContain(
      "The settings could not be loaded: Cannot reach the server",
    );
    await click(findByText(host, "button", "Try again"));
    await settle();
    flush();
    expect(control(form(), "Home repo").value).toBe("/home/me/code/notes");
  });

  it("saves the home repo, the scan roots as edited, and the clone root", async () => {
    const answer: Settings = {
      home_repo: "/home/me/code/home",
      scan_roots: ["/srv/git", "/opt/src", "/mnt/work"],
      clone_root: "/data/clones",
      max_parallel: null,
    };
    const putSettings = vi.fn<Api["putSettings"]>().mockResolvedValue(answer);
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue(catalog);
    const { host, form, roots, save } = await open({ putSettings, listRepos });

    type(control(form(), "Home repo"), "  /home/me/code/home ");
    await click(find(form(), 'button[aria-label="Remove /home/me/code"]'));
    expect(roots()).toEqual(["/srv/git"]);

    const add = control(form(), "Add a scan root");
    const addButton = findByText<HTMLButtonElement>(form(), "button", "Add");
    expect(addButton.disabled).toBe(true);
    type(add, " /opt/src ");
    await click(addButton);
    expect(roots()).toEqual(["/srv/git", "/opt/src"]);
    expect(add.value).toBe("");
    // Adding the same folder twice changes nothing.
    type(add, "/opt/src");
    add.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    flush();
    expect(roots()).toEqual(["/srv/git", "/opt/src"]);
    expect(putSettings).not.toHaveBeenCalled();
    // A path typed but not added is saved with the rest.
    type(add, "/mnt/work");
    type(control(form(), "Clone root"), "/data/clones");

    await save();
    expect(putSettings).toHaveBeenCalledWith(answer);
    expect(find(form(), '[role="status"]').textContent).toBe("Saved.");
    expect(roots()).toEqual(["/srv/git", "/opt/src", "/mnt/work"]);
    expect(control(form(), "Add a scan root").value).toBe("");
    expect(host.querySelector('[role="alert"]')).toBeNull();
    // A change re-reads the home repo: the repo list is read again.
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(2);

    // The next edit takes the confirmation away.
    type(control(form(), "Home repo"), "/elsewhere");
    expect(form().querySelector('[role="status"]')).toBeNull();
  });

  it("shows the form as the server stored it, and sends empty fields as not set", async () => {
    const putSettings = vi.fn<Api["putSettings"]>().mockResolvedValue({
      home_repo: null,
      scan_roots: [],
      clone_root: "/var/lib/calin/repos",
      max_parallel: null,
    });
    const { form, roots, save } = await open({
      getSettings: () =>
        Promise.resolve({ ...stored, clone_root: "/data/clones", max_parallel: null }),
      putSettings,
    });
    type(control(form(), "Home repo"), "   ");
    type(control(form(), "Clone root"), "");
    await click(find(form(), 'button[aria-label="Remove /home/me/code"]'));
    await click(find(form(), 'button[aria-label="Remove /srv/git"]'));
    expect(form().textContent).toContain("No scan root yet.");

    await save();
    expect(putSettings).toHaveBeenCalledWith(unset);
    expect(roots()).toEqual([]);
    // The server filled in its default: that is what the field shows.
    expect(control(form(), "Clone root").value).toBe("/var/lib/calin/repos");
  });

  it("disables Save while the request is in flight", async () => {
    const pending = deferred<Settings>();
    const putSettings = vi.fn<Api["putSettings"]>(() => pending.promise);
    const { form, save } = await open({ putSettings });
    await save();
    const button = findByText<HTMLButtonElement>(form(), "button", "Saving…");
    expect(button.disabled).toBe(true);
    await click(button);
    expect(putSettings).toHaveBeenCalledTimes(1);
    pending.resolve(stored);
    await click(form());
    expect(findByText<HTMLButtonElement>(form(), "button", "Save").disabled).toBe(false);
  });

  it.each([
    ["the home repo", "/home/me/code/notes is not the root of a git work tree", "Home repo"],
    ["the home repo, by name", "home_repo: no such directory", "Home repo"],
    ["a scan root", "scan root /srv/git is not an existing directory", "Add a scan root"],
    ["a scan root, by its path", "/home/me/code: not a directory", "Add a scan root"],
    ["the clone root", "clone_root: /nope cannot be created", "Clone root"],
  ])("shows a 400 about %s at that field", async (_name, message, label) => {
    const { host, form, save, errorAt, state } = await open({
      putSettings: () => Promise.reject(badRequest(message)),
    });
    type(control(form(), "Clone root"), "/nope");
    await save();

    expect(errorAt(label)).toBe(message);
    expect(control(form(), label).getAttribute("aria-invalid")).toBe("true");
    // Said once, where it belongs.
    expect(host.querySelectorAll('[role="alert"]')).toHaveLength(1);
    for (const other of ["Home repo", "Add a scan root", "Clone root"]) {
      if (other !== label) expect(errorAt(other)).toBeUndefined();
    }
    // What was typed is still there to be corrected, and nothing was stored.
    expect(control(form(), "Clone root").value).toBe("/nope");
    expect(state.settings()).toEqual({ status: "ready", settings: stored });
    expect(form().querySelector('[role="status"]')).toBeNull();
  });

  it("marks the scan root a 400 names", async () => {
    const { form, save } = await open({
      putSettings: () => Promise.reject(badRequest("scan root /srv/git does not exist")),
    });
    await save();
    const marked = [...form().querySelectorAll('ul[aria-label="Scan roots"] > li > span')].filter(
      (root) => root.classList.contains("text-tone-danger"),
    );
    expect(marked.map((root) => root.textContent)).toEqual(["/srv/git"]);
  });

  it.each([
    ["a 400 that points at no field", badRequest("invalid settings")],
    ["a refusal that is not about what was typed", conflict("a clone is running")],
    ["an outage", unreachable()],
    ["a failure that is not the server's", new HttpError(500, "Cannot reach the server", false)],
  ])("shows %s by the Save button, in the server's words", async (_name, error) => {
    const { host, form, save, errorAt } = await open({ putSettings: () => Promise.reject(error) });
    await save();
    const alert = find(form(), '[role="alert"]');
    expect(alert.textContent).toBe(error.message);
    expect(alert.parentElement).toBe(findByText(form(), "button", "Save").parentElement);
    expect(host.querySelectorAll('[role="alert"]')).toHaveLength(1);
    for (const label of ["Home repo", "Add a scan root", "Clone root"]) {
      expect(errorAt(label)).toBeUndefined();
    }
  });

  it("clears an error when the next save goes through", async () => {
    const putSettings = vi
      .fn<Api["putSettings"]>()
      .mockRejectedValueOnce(badRequest("home_repo: not a git work tree"))
      .mockResolvedValue(stored);
    const { form, save, errorAt } = await open({ putSettings });
    await save();
    expect(errorAt("Home repo")).toBe("home_repo: not a git work tree");
    await save();
    expect(errorAt("Home repo")).toBeUndefined();
    expect(control(form(), "Home repo").getAttribute("aria-invalid")).toBeNull();
  });

  it("sums up what the home repo declares: repos, found, missing, and the sets", async () => {
    const { summary } = await open();
    expect(summary().textContent).toContain("/home/me/code/notes");
    const facts = [...summary().querySelectorAll("dl > dt")].map((term) => [
      term.textContent,
      term.nextElementSibling?.textContent,
    ]);
    // The home repo itself and what was registered by hand are not declared.
    expect(facts).toEqual([
      ["Declared", "3 repos"],
      ["Found locally", "2"],
      ["Not cloned", "1"],
      ["Sets", "storefront (2 repos)solo (1 repo)"],
    ]);
    expect(summary().querySelector('[role="alert"]')).toBeNull();
    // Read-only: the only thing to press is Rescan.
    expect([...summary().querySelectorAll("button")].map((one) => one.textContent)).toEqual([
      "Rescan",
    ]);
  });

  it("shows the home repo's problem, and says when it declares no set", async () => {
    const { summary } = await open({
      listRepos: () =>
        Promise.resolve([
          { ...home, problem: ".calin/repos.yaml: did not find expected key at line 4" },
          makeRepo("scratch"),
        ]),
      listRepoSets: () => Promise.resolve([]),
    });
    expect(find(summary(), '[role="alert"]').textContent).toBe(
      ".calin/repos.yaml: did not find expected key at line 4",
    );
    expect(summary().textContent).toContain("Declared0 repos");
    expect(summary().textContent).toContain("Setsnone");
  });

  it("rescans on request and shows the new counts", async () => {
    const answer = deferred<Repo[]>();
    const refreshRepos = vi.fn<Api["refreshRepos"]>(() => answer.promise);
    const listRepoSets = vi.fn<Api["listRepoSets"]>().mockResolvedValue(sets);
    const { summary } = await open({ refreshRepos, listRepoSets });
    await click(findByText(summary(), "button", "Rescan"));
    const busy = findByText<HTMLButtonElement>(summary(), "button", "Rescanning…");
    expect(busy.disabled).toBe(true);
    expect(refreshRepos).toHaveBeenCalledTimes(1);

    // The clone of web was found this time.
    answer.resolve(
      catalog.map((repo) => (repo.id === "web" ? makeRepo("web", { declared: true }) : repo)),
    );
    await settle();
    flush();
    expect(findByText<HTMLButtonElement>(summary(), "button", "Rescan").disabled).toBe(false);
    expect(summary().textContent).toContain("Found locally3");
    expect(summary().textContent).toContain("Not cloned0");
    expect(listRepoSets).toHaveBeenCalledTimes(2);
  });

  it("shows the server's message when a rescan fails, and keeps the last counts", async () => {
    const { summary } = await open({
      refreshRepos: () => Promise.reject(conflict("a scan is already running")),
    });
    await click(findByText(summary(), "button", "Rescan"));
    expect(find(summary(), '[role="alert"]').textContent).toBe(
      "Could not rescan: a scan is already running",
    );
    expect(summary().textContent).toContain("Declared3 repos");
  });

  it("says when the repo list cannot be read, and tries again", async () => {
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(catalog);
    const { summary } = await open({ listRepos });
    expect(find(summary(), '[role="alert"]').textContent).toContain(
      "The repo list could not be loaded: Cannot reach the server",
    );
    await click(findByText(summary(), "button", "Retry"));
    await settle();
    flush();
    expect(summary().textContent).toContain("Declared3 repos");
  });

  it("explains what a home repo is, with an example, while none is set", async () => {
    const { host, summary, form } = await open({
      getSettings: () => Promise.resolve(unset),
      listRepos: () => Promise.resolve([makeRepo("scratch")]),
    });
    expect(control(form(), "Home repo").value).toBe("");
    const text = summary().textContent;
    expect(text).toContain("A home repo is the git repo you start every session from.");
    expect(text).toContain("It lists your other repos by name in .calin/repos.yaml");
    const example = find(summary(), "pre").textContent;
    expect(example).toContain("# <home repo>/.calin/repos.yaml");
    expect(example).toContain("repos:\n  api: { url: git@github.com:you/api.git }");
    expect(example).toContain("sets:\n  storefront: [api, web]");
    // Nothing to sum up, and nothing to rescan.
    expect(summary().querySelector("dl")).toBeNull();
    expect(host.textContent).not.toContain("Rescan");
  });

  it("swaps the explanation for the summary once a home repo is saved", async () => {
    const { form, save, summary } = await open({
      getSettings: () => Promise.resolve(unset),
      putSettings: (sent) => Promise.resolve(sent),
    });
    expect(summary().querySelector("pre")).not.toBeNull();
    type(control(form(), "Home repo"), "/home/me/code/notes");
    await save();
    await settle();
    flush();
    expect(summary().querySelector("pre")).toBeNull();
    expect(summary().textContent).toContain("Declared3 repos");
  });
});

describe("SettingsPage: workflows and the queue", () => {
  it("lists every workflow with its id, name and steps, and each problem of an invalid one", async () => {
    const { host } = await open({
      listWorkflows: () =>
        Promise.resolve([
          quickWorkflow,
          workflowInfo("implement-test-review", { name: "Implement, test, review" }),
          workflowInfo("nightly", {
            name: "Nightly",
            graph: null,
            problem: "line 4: unknown key `prompts`\nnode `ship`: not reachable from start",
          }),
        ]),
    });
    const items = [...host.querySelectorAll<HTMLElement>('ul[aria-label="Workflows"] > li')];
    expect(items.map((item) => item.getAttribute("data-workflow"))).toEqual([
      "quick",
      "implement-test-review",
      "nightly",
    ]);
    expect(items[0].textContent).toContain("Quick");
    expect(items[0].textContent).toContain("1 step");
    expect(items[0].textContent).toContain("Built in");
    expect(items[1].textContent).toContain("Implement, test, review");
    expect(items[1].textContent).toContain("implement-test-review");
    expect(items[1].textContent).toContain("7 steps");
    expect(items[1].hasAttribute("data-invalid")).toBe(false);

    expect(items[2].hasAttribute("data-invalid")).toBe(true);
    expect(items[2].textContent).toContain("Has errors");
    const problems = find(items[2], 'ul[aria-label="Problems in nightly"]');
    expect(problems.className).toContain("font-mono");
    expect([...problems.querySelectorAll("li")].map((line) => line.textContent)).toEqual([
      "line 4: unknown key `prompts`",
      "node `ship`: not reachable from start",
    ]);
  });

  it("says when the workflows cannot be read, in the server's words, and tries again", async () => {
    const listWorkflows = vi
      .fn<Api["listWorkflows"]>()
      .mockRejectedValueOnce(badRequest("the home repo cannot be read"))
      .mockResolvedValue([quickWorkflow]);
    const { host } = await open({ listWorkflows });
    expect(host.textContent).toContain(
      "The workflows could not be loaded: the home repo cannot be read",
    );
    await click(findByText(host, "button", "Retry"));
    await settle();
    flush();
    expect(host.querySelectorAll('ul[aria-label="Workflows"] > li')).toHaveLength(1);
  });

  it("shows and saves how many sessions run at once, as a number", async () => {
    const putSettings = vi
      .fn<Api["putSettings"]>()
      .mockImplementation((body) => Promise.resolve(body));
    const { form, save } = await open({
      getSettings: () => Promise.resolve({ ...stored, max_parallel: 4 }),
      putSettings,
    });
    const field = control(form(), "Sessions running at once");
    expect(field.value).toBe("4");
    type(field, "6");
    await save();
    expect(putSettings.mock.calls[0][0].max_parallel).toBe(6);

    type(field, "");
    await save();
    expect(putSettings.mock.calls[1][0].max_parallel).toBeNull();
  });

  it("shows a 400 about the limit at its field, in the server's words", async () => {
    const message = "max_parallel must be between 1 and 64";
    const { host, form, save } = await open({
      putSettings: () => Promise.reject(badRequest(message)),
    });
    const field = control(form(), "Sessions running at once");
    type(field, "0");
    await save();
    expect(field.getAttribute("aria-invalid")).toBe("true");
    const alerts = [...host.querySelectorAll('[role="alert"]')];
    expect(alerts.map((alert) => alert.textContent)).toEqual([message]);
    expect(field.getAttribute("aria-describedby")?.split(" ")).toContain(alerts[0].id);
  });
});
