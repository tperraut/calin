import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import type { RepoRegistry } from "../state/app";
import type { WorkflowInfo } from "../gen/WorkflowInfo";
import { conflict, deferred, makeMissingRepo, makeRepo, quickWorkflow } from "../test/fakes";
import { workflowInfo } from "../test/workflows";
import {
  choose,
  click,
  control,
  find,
  findByText,
  mount,
  PHONE,
  setViewportWidth,
  type,
  unmountAll,
} from "../test/render";
import { Composer } from "./Composer";

afterEach(unmountAll);

const threeRepos = [makeRepo("api"), makeRepo("docs"), makeRepo("web")];

function setup(
  options: {
    repos?: Repo[];
    registry?: RepoRegistry;
    sets?: RepoSet[];
    onCloneRepo?: (id: string) => Promise<void>;
    onCreate?: (request: CreateSessionRequest) => Promise<void>;
    onRegisterRepo?: (request: RegisterRepoRequest) => Promise<Repo>;
    onRemoveRepo?: (id: string) => Promise<void>;
  } = {},
) {
  const [registry, setRegistry] = createSignal<RepoRegistry>(
    options.registry ?? { status: "ready", repos: options.repos ?? threeRepos },
  );
  const onCreate = vi.fn(options.onCreate ?? (() => Promise.resolve()));
  const onRegisterRepo = vi.fn(
    options.onRegisterRepo ?? (() => Promise.reject(new Error("not stubbed"))),
  );
  const onRemoveRepo = vi.fn(options.onRemoveRepo ?? (() => Promise.resolve()));
  const onCloneRepo = vi.fn(options.onCloneRepo ?? (() => Promise.resolve()));
  const onRetryRepos = vi.fn();
  const host = mount(() => (
    <Composer
      runners={["mock", "claude"]}
      registry={registry()}
      sets={options.sets ?? []}
      onCreate={onCreate}
      onRegisterRepo={onRegisterRepo}
      onRemoveRepo={onRemoveRepo}
      onCloneRepo={onCloneRepo}
      onRetryRepos={onRetryRepos}
    />
  ));
  const rows = (): HTMLElement[] => [...host.querySelectorAll<HTMLElement>("section ul > li")];
  /** The names in the list, top to bottom. */
  const names = (): string[] => rows().map((row) => find(row, "label span span span").textContent);
  const repoRow = (name: string): HTMLElement => {
    const row = rows().find((one) => find(one, "label span span span").textContent === name);
    if (!row) throw new Error(`no row for ${name}`);
    return row;
  };
  /** Starts a session with what is on screen and returns the request that was built. */
  async function start(prompt = "Fix the login"): Promise<CreateSessionRequest> {
    type(find<HTMLTextAreaElement>(host, "textarea"), prompt);
    await click(findByText(host, "button", "Start"));
    const request = onCreate.mock.lastCall?.[0];
    if (!request) throw new Error("no session was started");
    return request;
  }
  return {
    host,
    setRegistry,
    onCreate,
    onRegisterRepo,
    onRemoveRepo,
    onCloneRepo,
    onRetryRepos,
    names,
    repoRow,
    start,
    pick: (name: string) => click(control(repoRow(name), name)),
  };
}

describe("Composer", () => {
  it("starts with no repo, asking for every permission, on the default runner", async () => {
    const { host, start } = setup();
    expect(host.textContent).toContain("No repo selected: the agent works in an empty folder.");

    expect(await start("  Fix the login  ")).toEqual({
      prompt: "Fix the login",
      permission_mode: "ask",
      repos: [],
      runner: "mock",
    });
  });

  it("attaches the chosen repos in the order chosen, the first as primary", async () => {
    const { host, repoRow, pick, start } = setup();
    await pick("web");
    await pick("api");

    expect(repoRow("web").textContent).toContain("Primary");
    expect(repoRow("api").textContent).not.toContain("Primary");
    expect(host.textContent).toContain("The agent starts in web, the primary repo.");
    expect((await start()).repos).toEqual([
      { repo_id: "web", mode: "write" },
      { repo_id: "api", mode: "write" },
    ]);
  });

  it("makes the second repo primary on request", async () => {
    const { repoRow, pick, start } = setup();
    await pick("api");
    await pick("web");
    expect(repoRow("api").textContent).toContain("Primary");
    // The primary repo has nothing to be promoted to.
    expect(repoRow("api").textContent).not.toContain("Make primary");

    await click(findByText(repoRow("web"), "button", "Make primary"));

    expect(repoRow("web").textContent).toContain("Primary");
    expect(repoRow("api").textContent).toContain("Make primary");
    expect((await start()).repos).toEqual([
      { repo_id: "web", mode: "write" },
      { repo_id: "api", mode: "write" },
    ]);
  });

  it("attaches a repo as a worktree unless told to edit it in place", async () => {
    const { repoRow, pick, start } = setup();
    await pick("api");
    await pick("docs");
    const mode = find<HTMLSelectElement>(repoRow("docs"), "select");
    expect([...mode.options].map((option) => option.textContent)).toEqual([
      "Worktree",
      "Read only",
      "Edit in place",
    ]);
    expect(mode.value).toBe("write");

    choose(mode, "live");

    expect((await start()).repos).toEqual([
      { repo_id: "api", mode: "write" },
      { repo_id: "docs", mode: "live" },
    ]);
  });

  it.each(["write", "read", "live"] as const)("sends a repo attached as %s", async (mode) => {
    const { repoRow, pick, start } = setup();
    await pick("api");
    await pick("docs");
    choose(find<HTMLSelectElement>(repoRow("docs"), "select"), mode);
    expect(find<HTMLSelectElement>(repoRow("docs"), "select").value).toBe(mode);
    expect((await start()).repos).toEqual([
      { repo_id: "api", mode: "write" },
      { repo_id: "docs", mode },
    ]);
  });

  it("says calmly when the primary repo is read only, and stops once it is not", async () => {
    const { host, repoRow, pick, start } = setup();
    const note = () => host.querySelector('[data-part="read-only-primary"]');
    await pick("api");
    await pick("docs");
    expect(note()).toBeNull();
    choose(find<HTMLSelectElement>(repoRow("docs"), "select"), "read");
    // Only the primary repo is the agent's working directory.
    expect(note()).toBeNull();
    choose(find<HTMLSelectElement>(repoRow("api"), "select"), "read");
    expect(note()?.textContent?.replace(/\s+/g, " ").trim()).toBe(
      "The agent starts in a read-only repo and will have to ask before changing anything there.",
    );
    choose(find<HTMLSelectElement>(repoRow("api"), "select"), "write");
    expect(note()).toBeNull();
    choose(find<HTMLSelectElement>(repoRow("api"), "select"), "read");
    expect(note()).not.toBeNull();
    // A read-only primary is allowed: the session starts with it.
    expect((await start()).repos).toEqual([
      { repo_id: "api", mode: "read" },
      { repo_id: "docs", mode: "read" },
    ]);
    await click(findByText(repoRow("docs"), "button", "Make primary"));
    expect(note()).not.toBeNull();
    choose(find<HTMLSelectElement>(repoRow("docs"), "select"), "live");
    expect(note()).toBeNull();
  });

  it("restores a remembered read-only repo as read only", async () => {
    localStorage.setItem(
      "calin.composer.v2",
      JSON.stringify({
        repos: [
          { repoId: "web", mode: "live" },
          { repoId: "docs", mode: "read" },
          { repoId: "api", mode: "bogus" },
        ],
        permissionMode: "ask",
      }),
    );
    const { repoRow, start } = setup();
    expect(find<HTMLSelectElement>(repoRow("docs"), "select").value).toBe("read");
    expect((await start()).repos).toEqual([
      { repo_id: "web", mode: "live" },
      { repo_id: "docs", mode: "read" },
      { repo_id: "api", mode: "write" },
    ]);
    unmountAll();

    const second = setup();
    expect(find<HTMLSelectElement>(second.repoRow("docs"), "select").value).toBe("read");
  });

  it("drops a repo that is unticked", async () => {
    const { pick, start } = setup();
    await pick("api");
    await pick("web");
    await pick("api");
    expect((await start()).repos).toEqual([{ repo_id: "web", mode: "write" }]);
  });

  it("offers three permission modes with what each one means, Ask first", async () => {
    const { host, start } = setup();
    const modes = [...host.querySelectorAll<HTMLInputElement>('input[type="radio"]')];
    expect(modes.map((mode) => [mode.value, mode.checked])).toEqual([
      ["ask", true],
      ["accept_edits", false],
      ["autonomous", false],
    ]);
    const fieldset = find(host, "fieldset");
    expect(fieldset.textContent).toContain("Every tool call waits for you.");
    expect(fieldset.textContent).toContain("File edits are allowed, commands wait.");
    expect(fieldset.textContent).toContain("Nothing waits, everything is still recorded.");

    await click(control(host, "Autonomous"));
    expect((await start()).permission_mode).toBe("autonomous");
  });

  it("keeps the runner and the model behind Options", async () => {
    const { host, start } = setup();
    const options = findByText<HTMLDetailsElement>(host, "details", "Options");
    expect(options.open).toBe(false);
    const model = find<HTMLInputElement>(options, 'input[type="text"]');
    expect(model.placeholder).toBe("runner default");

    expect(await start()).not.toHaveProperty("model");

    choose(find<HTMLSelectElement>(options, "select"), "claude");
    type(model, "  opus  ");
    expect(await start()).toMatchObject({ runner: "claude", model: "opus" });
  });

  it("starts the next session from the choices of the last one", async () => {
    const first = setup();
    await first.pick("web");
    await first.pick("api");
    choose(find<HTMLSelectElement>(first.repoRow("api"), "select"), "live");
    await click(control(first.host, "Accept edits"));
    choose(find<HTMLSelectElement>(first.host, "details select"), "claude");
    const sent = await first.start();
    unmountAll();

    const second = setup();
    expect(control(second.repoRow("web"), "web").checked).toBe(true);
    expect(second.repoRow("web").textContent).toContain("Primary");
    expect(find<HTMLSelectElement>(second.repoRow("api"), "select").value).toBe("live");
    expect(control(second.repoRow("docs"), "docs").checked).toBe(false);
    expect(control(second.host, "Accept edits").checked).toBe(true);
    expect(await second.start()).toEqual(sent);
  });

  it("does not remember a session that was refused", async () => {
    const first = setup({ onCreate: () => Promise.reject(new Error("runner is not available")) });
    await first.pick("api");
    await first.start();
    expect(find(first.host, '[role="alert"]').textContent).toBe("runner is not available");
    unmountAll();

    const second = setup();
    expect(control(second.repoRow("api"), "api").checked).toBe(false);
  });

  it("forgets a remembered repo that left the registry, and a runner the server dropped", async () => {
    localStorage.setItem(
      "calin.composer.v2",
      JSON.stringify({
        repos: [
          { repoId: "gone", mode: "live" },
          { repoId: "web", mode: "live" },
        ],
        permissionMode: "autonomous",
        runner: "codex",
      }),
    );
    const { repoRow, start } = setup();
    expect(repoRow("web").textContent).toContain("Primary");
    expect(await start()).toEqual({
      prompt: "Fix the login",
      permission_mode: "autonomous",
      repos: [{ repo_id: "web", mode: "live" }],
      runner: "mock",
    });
  });

  it("keeps the remembered repos when a session starts before the registry could be read", async () => {
    const first = setup();
    await first.pick("web");
    await first.start();
    unmountAll();

    const offline = setup({ registry: { status: "error", message: "Cannot reach the server" } });
    expect((await offline.start()).repos).toEqual([]);
    unmountAll();

    const back = setup();
    expect(control(back.repoRow("web"), "web").checked).toBe(true);
  });

  it("starts from the defaults when what is stored makes no sense", async () => {
    localStorage.setItem("calin.composer.v2", '{"repos":"api","permissionMode":"yolo","runner":7}');
    const { start } = setup();
    expect(await start()).toEqual({
      prompt: "Fix the login",
      permission_mode: "ask",
      repos: [],
      runner: "mock",
    });
  });

  it("works without storage", async () => {
    const blocked = (): never => {
      throw new DOMException("The operation is insecure.", "SecurityError");
    };
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(blocked);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(blocked);

    const { host, pick, start, onCreate } = setup();
    await pick("api");
    expect((await start()).repos).toEqual([{ repo_id: "api", mode: "write" }]);
    expect(onCreate).toHaveBeenCalledTimes(1);
    expect(host.querySelector('[role="alert"]')).toBeNull();
    // The prompt was sent, so the field is ready for the next one.
    expect(find<HTMLTextAreaElement>(host, "textarea").value).toBe("");
  });

  it("explains what a repo is when none is registered, and registers one", async () => {
    const api = makeRepo("api");
    const { host, setRegistry, onRegisterRepo, start } = setup({ repos: [] });
    expect(host.textContent).toContain("No repo is registered yet");
    expect(host.textContent).toContain("A repo is a git clone on the server's machine.");
    expect(host.querySelector('form[aria-label="Register a repo"]')).toBeNull();

    await click(findByText(host, "button", "Register a repo"));
    const form = find<HTMLFormElement>(host, 'form[aria-label="Register a repo"]');
    onRegisterRepo.mockImplementation((request) => {
      // What the app state does: the registry gains the repo the server answered with.
      setRegistry({ status: "ready", repos: [api] });
      return Promise.resolve({ ...api, path: request.path });
    });
    type(control(form, "Path on the server"), "  /code/api  ");
    await click(findByText(form, "button", "Register"));

    expect(onRegisterRepo).toHaveBeenCalledWith({ path: "/code/api" });
    expect(host.querySelector('form[aria-label="Register a repo"]')).toBeNull();
    // The repo that was just registered is the one the reader wants attached.
    expect((await start()).repos).toEqual([{ repo_id: "api", mode: "write" }]);
  });

  it("sends the optional name, and shows the server's message when it is taken", async () => {
    const { host, onRegisterRepo } = setup({
      onRegisterRepo: () => Promise.reject(conflict("a repo named api is already registered")),
    });
    await click(findByText(host, "button", "Register a repo"));
    const form = find<HTMLFormElement>(host, 'form[aria-label="Register a repo"]');
    const path = control(form, "Path on the server");
    type(path, "/srv/other/api");
    type(control(form, "Name"), "api");
    await click(findByText(form, "button", "Register"));

    expect(onRegisterRepo).toHaveBeenCalledWith({ path: "/srv/other/api", name: "api" });
    expect(find(form, '[role="alert"]').textContent).toBe("a repo named api is already registered");
    // Still open, with what was typed, so the name can be changed and sent again.
    expect(path.value).toBe("/srv/other/api");
    expect(findByText<HTMLButtonElement>(form, "button", "Register").disabled).toBe(false);
  });

  it("does not register a blank path, and disables the form while a request is in flight", async () => {
    const pending = deferred<Repo>();
    const { host, onRegisterRepo } = setup({ onRegisterRepo: () => pending.promise });
    await click(findByText(host, "button", "Register a repo"));
    const form = find<HTMLFormElement>(host, 'form[aria-label="Register a repo"]');
    const submit = findByText<HTMLButtonElement>(form, "button", "Register");

    type(control(form, "Path on the server"), "   ");
    await click(submit);
    expect(onRegisterRepo).not.toHaveBeenCalled();

    type(control(form, "Path on the server"), "/code/new");
    await click(submit);
    expect(submit.disabled).toBe(true);
    expect(submit.textContent).toBe("Registering…");

    pending.resolve(makeRepo("new"));
    await click(host);
    expect(host.querySelector('form[aria-label="Register a repo"]')).toBeNull();
  });

  it("removes a repo from the registry only after a confirmation", async () => {
    const { repoRow, onRemoveRepo } = setup();
    const row = repoRow("docs");
    await click(find(row, 'button[aria-label="Remove docs from the registry"]'));
    expect(onRemoveRepo).not.toHaveBeenCalled();
    expect(find(row, '[role="group"]').textContent).toContain("The clone on disk is not touched.");

    await click(findByText(row, "button", "Keep"));
    expect(row.querySelector('[role="group"]')).toBeNull();
    expect(onRemoveRepo).not.toHaveBeenCalled();

    await click(find(row, 'button[aria-label="Remove docs from the registry"]'));
    await click(findByText(row, "button", "Remove from registry"));
    expect(onRemoveRepo).toHaveBeenCalledWith("docs");
  });

  it("shows the server's message when a repo cannot be removed", async () => {
    const { repoRow } = setup({
      onRemoveRepo: () =>
        Promise.reject(conflict("api is attached to a session that is still open")),
    });
    const row = repoRow("api");
    await click(find(row, 'button[aria-label="Remove api from the registry"]'));
    await click(findByText(row, "button", "Remove from registry"));

    expect(find(row, '[role="alert"]').textContent).toBe(
      "api is attached to a session that is still open",
    );
    expect(findByText<HTMLButtonElement>(row, "button", "Remove from registry").disabled).toBe(
      false,
    );
  });

  it("stops attaching a repo once it is removed from the registry", async () => {
    const { setRegistry, pick, start } = setup();
    await pick("api");
    await pick("web");
    setRegistry({ status: "ready", repos: [makeRepo("docs"), makeRepo("web")] });
    flush();
    expect((await start()).repos).toEqual([{ repo_id: "web", mode: "write" }]);
  });

  it("says when the registry is loading or could not be read, and offers a retry", async () => {
    const { host, setRegistry, onRetryRepos, start } = setup({ registry: { status: "loading" } });
    expect(host.textContent).toContain("Loading repos…");
    // A session can still be started; it just attaches nothing.
    expect((await start()).repos).toEqual([]);

    setRegistry({ status: "error", message: "Cannot reach the server" });
    flush();
    expect(host.textContent).toContain("The repos could not be loaded: Cannot reach the server");
    await click(findByText(host, "button", "Retry"));
    expect(onRetryRepos).toHaveBeenCalledTimes(1);
  });
});

describe("Composer: the catalog", () => {
  const home = makeRepo("notes", { home: true });
  const api = makeRepo("api", { declared: true, url: "git@example.com:you/api.git" });
  const web = makeMissingRepo("web");
  const tools = makeRepo("tools");
  const catalog = [home, api, tools, web];
  const box = (row: HTMLElement) => find<HTMLInputElement>(row, 'input[type="checkbox"]');

  it("lists the home repo first, marked Home, and says where a repo is only when that helps", () => {
    const { names, repoRow } = setup({ repos: catalog });
    expect(names()).toEqual(["notes", "api", "tools", "web"]);
    expect(repoRow("notes").textContent).toContain("Home");
    expect(repoRow("api").textContent).not.toContain("Home");
    // An ordinary repo: its name and its path, nothing about where it comes from.
    expect(find(repoRow("api"), "label").textContent).toBe("api/code/api");
    expect(find(repoRow("tools"), "label").textContent).toBe("tools/code/tools");
    expect(find(repoRow("web"), "label").textContent).toBe("webnot cloned");
  });

  it("starts in the home repo, primary and edited in place, when nothing is remembered", async () => {
    const { host, repoRow, start } = setup({ repos: catalog });
    expect(box(repoRow("notes")).checked).toBe(true);
    expect(repoRow("notes").textContent).toContain("Primary");
    expect(find<HTMLSelectElement>(repoRow("notes"), "select").value).toBe("live");
    expect(host.textContent).toContain("The agent starts in notes, the primary repo.");
    expect(box(repoRow("api")).checked).toBe(false);

    expect((await start()).repos).toEqual([{ repo_id: "notes", mode: "live" }]);
  });

  it("adds the other repos after the home repo, as worktrees", async () => {
    const { pick, start } = setup({ repos: catalog });
    await pick("tools");
    await pick("api");
    expect((await start()).repos).toEqual([
      { repo_id: "notes", mode: "live" },
      { repo_id: "tools", mode: "write" },
      { repo_id: "api", mode: "write" },
    ]);
  });

  it("lets the reader untick the home repo, and remembers that they did", async () => {
    const first = setup({ repos: catalog });
    await first.pick("notes");
    expect(box(first.repoRow("notes")).checked).toBe(false);
    expect((await first.start()).repos).toEqual([]);
    unmountAll();

    // An empty selection the reader made is a selection: the default does not come back over it.
    const second = setup({ repos: catalog });
    expect(box(second.repoRow("notes")).checked).toBe(false);
    // Ticked again, it is the primary repo again, edited in place.
    await second.pick("api");
    await second.pick("notes");
    expect((await second.start()).repos).toEqual([
      { repo_id: "notes", mode: "live" },
      { repo_id: "api", mode: "write" },
    ]);
  });

  it("has no default without a home repo, or while the home repo is not available", async () => {
    const none = setup({ repos: [api, tools] });
    expect((await none.start()).repos).toEqual([]);
    unmountAll();

    const missing = setup({ repos: [{ ...home, available: false, path: "" }, api] });
    expect(box(missing.repoRow("notes")).checked).toBe(false);
    expect((await missing.start()).repos).toEqual([]);
  });

  it("applies the default once the catalog arrives", async () => {
    const { setRegistry, repoRow, start } = setup({ registry: { status: "loading" } });
    setRegistry({ status: "ready", repos: catalog });
    flush();
    expect(box(repoRow("notes")).checked).toBe(true);
    expect((await start()).repos).toEqual([{ repo_id: "notes", mode: "live" }]);
  });

  it("drops remembered repos that are gone or not cloned, and remembers what is left", async () => {
    localStorage.setItem(
      "calin.composer.v2",
      JSON.stringify({
        repos: [
          { repoId: "gone", mode: "write" },
          { repoId: "web", mode: "write" },
          { repoId: "tools", mode: "live" },
        ],
        permissionMode: "ask",
      }),
    );
    const { repoRow, start } = setup({ repos: catalog });
    expect(box(repoRow("web")).checked).toBe(false);
    expect(repoRow("tools").textContent).toContain("Primary");
    // Something of the remembered selection is left: the home default does not apply.
    expect(box(repoRow("notes")).checked).toBe(false);
    expect((await start()).repos).toEqual([{ repo_id: "tools", mode: "live" }]);
    expect(JSON.parse(localStorage.getItem("calin.composer.v2") ?? "{}")).toMatchObject({
      repos: [{ repoId: "tools", mode: "live" }],
    });
  });

  it("falls back to the home repo when nothing of the remembered selection is left", async () => {
    localStorage.setItem(
      "calin.composer.v2",
      JSON.stringify({ repos: [{ repoId: "gone", mode: "write" }], permissionMode: "ask" }),
    );
    const { start } = setup({ repos: catalog });
    expect((await start()).repos).toEqual([{ repo_id: "notes", mode: "live" }]);
  });

  it("keeps the mode and the runner of a session started before there was a home repo, not its repos", async () => {
    localStorage.setItem(
      "calin.composer.v1",
      JSON.stringify({
        repos: [{ repoId: "tools", mode: "write" }],
        permissionMode: "accept_edits",
        runner: "claude",
      }),
    );
    const { start } = setup({ repos: catalog });
    expect(await start()).toEqual({
      prompt: "Fix the login",
      permission_mode: "accept_edits",
      repos: [{ repo_id: "notes", mode: "live" }],
      runner: "claude",
    });
  });

  it("does not let a repo that is not cloned be selected", async () => {
    const { repoRow, start } = setup({ repos: catalog });
    const row = repoRow("web");
    expect(box(row).disabled).toBe(true);
    expect(row.textContent).toContain("not cloned");
    await click(box(row));
    expect(box(row).checked).toBe(false);
    expect(row.querySelector("select")).toBeNull();
    expect((await start()).repos).toEqual([{ repo_id: "notes", mode: "live" }]);
  });

  it("offers to clone a declared repo that has a url, and shows the clone running", async () => {
    const asked = deferred();
    const noUrl = makeMissingRepo("vendor", { url: null });
    const { repoRow, setRegistry, onCloneRepo } = setup({
      repos: [...catalog, noUrl],
      onCloneRepo: () => asked.promise,
    });
    // Nothing to clone from: the row says so by offering nothing.
    expect(repoRow("vendor").textContent).toContain("not cloned");
    expect(repoRow("vendor").querySelector("button")).toBeNull();
    // An available repo is not cloned again.
    expect(repoRow("api").querySelector('button[aria-label="Clone api"]')).toBeNull();

    await click(find(repoRow("web"), 'button[aria-label="Clone web"]'));
    expect(onCloneRepo).toHaveBeenCalledWith("web");
    expect(find(repoRow("web"), '[role="status"]').textContent).toBe("Cloning…");
    expect(repoRow("web").querySelector('button[aria-label="Clone web"]')).toBeNull();

    // The server took it: the catalog says the clone is running.
    setRegistry({ status: "ready", repos: [home, api, tools, { ...web, cloning: true }] });
    asked.resolve();
    await click(repoRow("web"));
    expect(find(repoRow("web"), '[role="status"]').textContent).toBe("Cloning…");
    expect(box(repoRow("web")).disabled).toBe(true);

    // Done: it is a repo like the others.
    setRegistry({
      status: "ready",
      repos: [home, api, tools, makeRepo("web", { declared: true, url: web.url })],
    });
    flush();
    expect(repoRow("web").querySelector('[role="status"]')).toBeNull();
    expect(repoRow("web").textContent).not.toContain("not cloned");
    expect(box(repoRow("web")).disabled).toBe(false);
  });

  it("shows why a clone failed, from the request or from the repo, and offers it again", async () => {
    const { repoRow, setRegistry, onCloneRepo } = setup({
      repos: catalog,
      onCloneRepo: () => Promise.reject(conflict("web is already being cloned")),
    });
    await click(find(repoRow("web"), 'button[aria-label="Clone web"]'));
    expect(find(repoRow("web"), '[role="alert"]').textContent).toBe("web is already being cloned");
    expect(repoRow("web").querySelector('[role="status"]')).toBeNull();

    // The clone itself failed: the repo carries the reason.
    setRegistry({
      status: "ready",
      repos: [home, api, tools, { ...web, problem: "git clone failed: repository not found" }],
    });
    flush();
    expect(repoRow("web").textContent).toContain("git clone failed: repository not found");
    onCloneRepo.mockResolvedValue();
    await click(find(repoRow("web"), 'button[aria-label="Clone web"]'));
    expect(onCloneRepo).toHaveBeenCalledTimes(2);
    expect(repoRow("web").querySelector('[role="alert"]')).toBeNull();
  });

  it("shows a repo's problem even when the repo can be used", () => {
    const { repoRow } = setup({
      repos: [{ ...home, problem: ".calin/repos.yaml: mapping values are not allowed here" }],
    });
    expect(repoRow("notes").textContent).toContain("mapping values are not allowed here");
    expect(box(repoRow("notes")).disabled).toBe(false);
  });

  it("offers Remove for a repo registered by hand only", () => {
    const { repoRow } = setup({ repos: catalog });
    const remove = (name: string) =>
      repoRow(name).querySelector(`button[aria-label="Remove ${name} from the registry"]`);
    expect(remove("tools")).not.toBeNull();
    // What the home repo declares goes when it stops declaring it; the home repo is a setting.
    expect(remove("api")).toBeNull();
    expect(remove("web")).toBeNull();
    expect(remove("notes")).toBeNull();
  });

  describe("sets", () => {
    const docs = makeRepo("docs", { declared: true });
    const sets: RepoSet[] = [
      { name: "storefront", repo_ids: ["api", "web", "docs"] },
      { name: "all", repo_ids: ["api", "docs", "tools", "unknown"] },
    ];
    const withSets = () => setup({ repos: [home, api, docs, tools, web], sets });
    const chip = (host: HTMLElement, name: string) =>
      findByText<HTMLButtonElement>(find(host, '[aria-label="Repo sets"]'), "button", name);

    it("are offered as chips, and none without a home repo that has some", () => {
      const { host } = withSets();
      const chips = [...find(host, '[aria-label="Repo sets"]').querySelectorAll("button")];
      // How many of its repos can be attached, out of how many when some cannot.
      expect(chips.map((one) => one.textContent)).toEqual(["storefront2/3", "all3"]);
      expect(chips.map((one) => one.getAttribute("aria-pressed"))).toEqual(["false", "false"]);
      unmountAll();
      expect(setup({ repos: catalog }).host.querySelector('[aria-label="Repo sets"]')).toBeNull();
    });

    it("add their available repos as worktrees to what is selected, and say what was left out", async () => {
      const { host, pick, start, setRegistry } = withSets();
      await pick("docs");
      choose(find<HTMLSelectElement>(host, "ul > li:nth-child(3) select"), "live");

      await click(chip(host, "storefront"));
      expect(chip(host, "storefront").getAttribute("aria-pressed")).toBe("true");
      expect(find(host, 'section [role="status"]').textContent).toBe(
        "storefront: web is not cloned and was left out.",
      );
      // Once the missing repo is cloned there is nothing left to say.
      setRegistry({
        status: "ready",
        repos: [home, api, docs, tools, makeRepo("web", { declared: true })],
      });
      flush();
      expect(host.querySelector('section [role="status"]')).toBeNull();
      // What was selected stays as it was; the set's other repos join as worktrees.
      expect((await start()).repos).toEqual([
        { repo_id: "notes", mode: "live" },
        { repo_id: "docs", mode: "live" },
        { repo_id: "api", mode: "write" },
      ]);
    });

    it("remove their repos when chosen again", async () => {
      const { host, pick, start } = withSets();
      await click(chip(host, "all"));
      // Every repo of storefront that can be attached is selected too.
      expect(chip(host, "storefront").getAttribute("aria-pressed")).toBe("true");
      expect(host.querySelector('section [role="status"]')).toBeNull();
      expect((await start()).repos.map((repo) => repo.repo_id)).toEqual([
        "notes",
        "api",
        "docs",
        "tools",
      ]);

      await click(chip(host, "storefront"));
      expect(chip(host, "storefront").getAttribute("aria-pressed")).toBe("false");
      expect(chip(host, "all").getAttribute("aria-pressed")).toBe("false");
      expect((await start()).repos.map((repo) => repo.repo_id)).toEqual(["notes", "tools"]);

      // Unticking one of a set's repos by hand turns the chip off.
      await click(chip(host, "storefront"));
      await pick("api");
      expect(chip(host, "storefront").getAttribute("aria-pressed")).toBe("false");
    });

    it("say so when none of a set's repos is cloned, and select nothing", async () => {
      const { host, start } = setup({
        repos: [home, web, makeMissingRepo("mobile")],
        sets: [{ name: "apps", repo_ids: ["web", "mobile"] }],
      });
      await click(chip(host, "apps"));
      expect(chip(host, "apps").getAttribute("aria-pressed")).toBe("false");
      expect(find(host, 'section [role="status"]').textContent).toBe(
        "apps: web and mobile are not cloned and were left out.",
      );
      expect((await start()).repos).toEqual([{ repo_id: "notes", mode: "live" }]);
    });
  });

  describe("with many repos", () => {
    const many = [
      home,
      ...["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "Theta"].map((name) =>
        makeRepo(name),
      ),
    ];

    it("gets a filter box from nine repos on, and none below", () => {
      expect(setup({ repos: many }).host.querySelector('input[type="search"]')).not.toBeNull();
      unmountAll();
      expect(
        setup({ repos: many.slice(0, 8) }).host.querySelector('input[type="search"]'),
      ).toBeNull();
    });

    it("filters by name, whatever the case, and keeps what is selected selected", async () => {
      const { host, names, pick, start } = setup({ repos: many });
      const filter = control(host, "Filter repos");
      await pick("beta");

      type(filter, "  ETA ");
      expect(names()).toEqual(["beta", "zeta", "eta", "Theta"]);
      await pick("zeta");

      type(filter, "nothing like it");
      expect(names()).toEqual([]);
      expect(host.textContent).toContain('No repo matches "nothing like it".');

      type(filter, "");
      expect(names()).toHaveLength(9);
      expect((await start()).repos.map((repo) => repo.repo_id)).toEqual(["notes", "beta", "zeta"]);
    });
  });

  describe("on a phone", () => {
    const fold = (host: HTMLElement) =>
      find<HTMLButtonElement>(host, "section button[aria-controls]");
    const folded = (host: HTMLElement): boolean => {
      const body = document.getElementById(fold(host).getAttribute("aria-controls") ?? "");
      if (!body) throw new Error("the fold controls nothing");
      return body.hidden === true;
    };

    it("folds the list behind what is selected, so the prompt and Start stay in reach", async () => {
      localStorage.setItem(
        "calin.composer.v2",
        JSON.stringify({
          repos: [
            { repoId: "notes", mode: "live" },
            { repoId: "api", mode: "write" },
            { repoId: "tools", mode: "write" },
          ],
          permissionMode: "ask",
        }),
      );
      setViewportWidth(PHONE);
      const { host, start } = setup({ repos: catalog });
      expect(fold(host).textContent).toContain("notes + 2 repos");
      expect(fold(host).getAttribute("aria-expanded")).toBe("false");
      expect(folded(host)).toBe(true);
      // Nothing of the list stands between the prompt and Start.
      expect(find(host, "ul").closest("[hidden]")).not.toBeNull();

      // Folded or not, what is selected is what is attached.
      expect((await start()).repos).toHaveLength(3);

      await click(fold(host));
      expect(fold(host).getAttribute("aria-expanded")).toBe("true");
      expect(folded(host)).toBe(false);
      await click(fold(host));
      expect(folded(host)).toBe(true);
    });

    it("folds on the home repo alone, and says so", () => {
      setViewportWidth(PHONE);
      const { host } = setup({ repos: catalog });
      expect(fold(host).textContent).toContain("notes");
      expect(fold(host).textContent).not.toContain("+");
      expect(folded(host)).toBe(true);
    });

    it("stays open while nothing is selected, and under the reader's hand once something is", async () => {
      setViewportWidth(PHONE);
      const { host, pick } = setup({ repos: [api, tools] });
      expect(fold(host).textContent).toContain("none selected");
      expect(folded(host)).toBe(false);

      await pick("api");
      await pick("tools");
      // Picking does not fold the list away mid-choice.
      expect(folded(host)).toBe(false);
      expect(fold(host).textContent).toContain("api + 1 repo");
    });

    it("is not folded on a wide screen", () => {
      const { host } = setup({ repos: catalog });
      expect(host.querySelector("section button[aria-controls]")).toBeNull();
      expect(find(host, "ul").closest("[hidden]")).toBeNull();
    });
  });
});

describe("Composer: workflows", () => {
  const itr = workflowInfo("implement-test-review", {
    name: "Implement, test, review",
    description: "Implements, runs the checks until they pass, reviews, asks before shipping.",
    budget_usd: 5,
  });
  const broken = workflowInfo("broken", { name: "Broken", graph: null, problem: "line 3: bad" });

  function mountWith(workflows: readonly WorkflowInfo[]) {
    const onCreate = vi.fn((_request: CreateSessionRequest) => Promise.resolve());
    const host = mount(() => (
      <Composer
        runners={["mock"]}
        registry={{ status: "ready", repos: [] }}
        sets={[]}
        workflows={{ status: "ready", workflows }}
        onCreate={onCreate}
        onRegisterRepo={() => Promise.reject(new Error("not stubbed"))}
        onRemoveRepo={() => Promise.resolve()}
        onCloneRepo={() => Promise.resolve()}
        onRetryRepos={() => {}}
      />
    ));
    const select = () => control<HTMLSelectElement>(host, "Workflow");
    async function start(): Promise<CreateSessionRequest> {
      type(find<HTMLTextAreaElement>(host, "textarea"), "Fix the login");
      await click(findByText(host, "button", "Start"));
      const request = onCreate.mock.lastCall?.[0];
      if (!request) throw new Error("no session was started");
      return request;
    }
    return { host, select, start };
  }

  it("offers no choice when only the built-in workflow exists, and sends none", async () => {
    const { host, start } = mountWith([quickWorkflow]);
    expect([...host.querySelectorAll("label")].some((one) => one.textContent === "Workflow")).toBe(
      false,
    );
    expect(await start()).not.toHaveProperty("workflow");
  });

  it("starts on Quick, lists an invalid workflow disabled, and sends none for Quick", async () => {
    const { select, start } = mountWith([quickWorkflow, broken, itr]);
    const options = [...select().options];
    expect(options.map((one) => [one.value, one.textContent, one.disabled])).toEqual([
      ["quick", "Quick (no workflow)", false],
      ["broken", "Broken (has errors)", true],
      ["implement-test-review", "Implement, test, review", false],
    ]);
    expect(select().value).toBe("quick");
    expect(await start()).not.toHaveProperty("workflow");
  });

  it("describes the chosen workflow: what it does, its budget and its shape", () => {
    const { host, select } = mountWith([quickWorkflow, itr]);
    choose(select(), "implement-test-review");
    const note = document.getElementById(select().getAttribute("aria-describedby") ?? "");
    expect(note?.textContent).toContain("Implements, runs the checks until they pass");
    expect(note?.textContent).toContain("Budget: $5.00");
    expect(find(host, '[data-part="workflow-shape"]').textContent).toBe(
      "7 steps: 3 agent, 1 check, 1 loop, 1 gate, 1 ship",
    );
  });

  it("sends the chosen workflow and remembers it for the next session", async () => {
    const { select, start } = mountWith([quickWorkflow, itr]);
    choose(select(), "implement-test-review");
    expect((await start()).workflow).toBe("implement-test-review");
    expect(JSON.parse(localStorage.getItem("calin.composer.v2") ?? "{}").workflow).toBe(
      "implement-test-review",
    );
    unmountAll();
    expect(mountWith([quickWorkflow, itr]).select().value).toBe("implement-test-review");
  });

  it.each([
    ["became invalid", [quickWorkflow, { ...itr, graph: null, problem: "line 1: bad" }]],
    ["no longer exists", [quickWorkflow, broken]],
  ])("drops the remembered workflow when it %s", async (_case, workflows) => {
    localStorage.setItem(
      "calin.composer.v2",
      JSON.stringify({ permissionMode: "ask", workflow: "implement-test-review" }),
    );
    const { select, start } = mountWith(workflows);
    expect(select().value).toBe("quick");
    expect(await start()).not.toHaveProperty("workflow");
    expect(JSON.parse(localStorage.getItem("calin.composer.v2") ?? "{}").workflow).toBeUndefined();
  });
});
