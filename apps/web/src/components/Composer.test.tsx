import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoRegistry } from "../state/app";
import { conflict, deferred, makeRepo } from "../test/fakes";
import { choose, click, control, find, findByText, mount, type, unmountAll } from "../test/render";
import { Composer } from "./Composer";

afterEach(unmountAll);

const threeRepos = [makeRepo("api"), makeRepo("docs"), makeRepo("web")];

function setup(
  options: {
    repos?: Repo[];
    registry?: RepoRegistry;
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
  const onRetryRepos = vi.fn();
  const host = mount(() => (
    <Composer
      runners={["mock", "claude"]}
      registry={registry()}
      onCreate={onCreate}
      onRegisterRepo={onRegisterRepo}
      onRemoveRepo={onRemoveRepo}
      onRetryRepos={onRetryRepos}
    />
  ));
  const repoRow = (name: string): HTMLElement => {
    const row = findByText(host, "label", `/code/${name}`).closest("li");
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
    onRetryRepos,
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
      "Edit in place",
    ]);
    expect(mode.value).toBe("write");

    choose(mode, "live");

    expect((await start()).repos).toEqual([
      { repo_id: "api", mode: "write" },
      { repo_id: "docs", mode: "live" },
    ]);
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
      "calin.composer.v1",
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
    localStorage.setItem("calin.composer.v1", '{"repos":"api","permissionMode":"yolo","runner":7}');
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
