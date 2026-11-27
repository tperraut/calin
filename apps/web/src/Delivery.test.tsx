import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { EventBody } from "./gen/EventBody";
import type { Health } from "./gen/Health";
import type { PullRequest } from "./gen/PullRequest";
import type { RepoChanges } from "./gen/RepoChanges";
import type { SessionStatus } from "./gen/SessionStatus";
import { createAppState } from "./state/app";
import { resetFakeEditors } from "./test/fakeEditor";
import {
  conflict,
  deferred,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makePullRequest,
  makeSession,
  makeSessionRepo,
  settle,
} from "./test/fakes";
import {
  click,
  control,
  find,
  findByText,
  mount,
  offersButton,
  PHONE,
  setViewportWidth,
  type,
  unmountAll,
} from "./test/render";

// The Changes panel loads this in place of CodeMirror (see src/test/).
vi.mock("./panels/editor/codemirror", async () => ({
  default: (await import("./test/fakeEditor")).fakeEditorModule,
}));

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock"],
  github_problem: null,
};

const repos = [
  makeSessionRepo("api", { primary: true, branch: "calin/s1" }),
  makeSessionRepo("web", { branch: "calin/s1" }),
  makeSessionRepo("docs", { mode: "read" }),
  makeSessionRepo("notes", { mode: "live" }),
  makeSessionRepo("tools", { branch: "calin/s1" }),
];

const changed = (id: string, files: number, added: number, removed: number): RepoChanges => ({
  session_repo_id: id,
  name: id,
  branch: null,
  base_ref: null,
  files: Array.from({ length: files }, (_, index) => ({
    path: `src/f${index}.rs`,
    old_path: null,
    status: "modified",
    added: 1,
    removed: 0,
    binary: false,
  })),
  added,
  removed,
});

/** `api` and `web` changed; `docs` and `notes` did too but never ship; `tools` did not. */
const changes = [
  changed("api", 3, 40, 12),
  changed("web", 1, 5, 0),
  changed("docs", 2, 2, 2),
  changed("notes", 1, 1, 0),
  changed("tools", 0, 0, 0),
];

type Options = {
  routes?: Partial<Api>;
  status?: SessionStatus;
  pullRequests?: PullRequest[];
  path?: string;
  changes?: RepoChanges[];
};

/** Opens session `s1`, caught up, with its changes read: in review unless told otherwise. */
async function openSession(options: Options = {}) {
  const status = options.status ?? "review";
  history.replaceState(null, "", options.path ?? "/s/s1");
  const socket = new FakeSocket();
  const session = makeSession("s1", 1000, { title: "Add a health check", status });
  const shipSession = vi.fn<Api["shipSession"]>().mockResolvedValue(undefined);
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    getSession: () => Promise.resolve(makeDetail(session, repos, options.pullRequests ?? [])),
    getChanges: () => Promise.resolve(options.changes ?? changes),
    shipSession,
    ...options.routes,
  });
  const state = createAppState({ api, socket, schedule: immediately });
  const host = mount(() => <App state={state} />);
  await settle();
  flush();
  let seq = 0;
  const emit = (body: EventBody, agentId: string | null = null) => {
    seq += 1;
    socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(seq, body, agentId),
    });
    flush();
  };
  emit({ type: "session_status", status, reason: null });
  socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: seq });
  flush();
  // The changes list is read once the session is known, a moment later.
  await vi.advanceTimersByTimeAsync(500);
  await act();
  const main = find(host, "main");
  const header = find(main, "header");
  return { host, main, header, socket, emit, shipSession };
}

async function act(): Promise<void> {
  for (let round = 0; round < 4; round += 1) {
    await settle();
    flush();
  }
}

const opened = (repo: string, number: number, patch: Partial<PullRequest> = {}): EventBody => ({
  type: "pull_request_opened",
  pull_request: makePullRequest(repo, number, patch),
});
const updated = (repo: string, number: number, patch: Partial<PullRequest> = {}): EventBody => ({
  type: "pull_request_updated",
  pull_request: makePullRequest(repo, number, patch),
});

const delivery = (root: ParentNode) => root.querySelector<HTMLElement>('[aria-label="Delivery"]');
const action = (root: ParentNode) =>
  find<HTMLButtonElement>(root, '[aria-label="Delivery"] button');
const form = (root: ParentNode) =>
  root.querySelector<HTMLFormElement>('form[aria-label="Ship the session"]');
const openForm = async (root: HTMLElement): Promise<HTMLFormElement> => {
  await click(action(root));
  const shown = form(root);
  if (!shown) throw new Error("the ship form did not open");
  return shown;
};
const planLines = (shown: HTMLElement) =>
  [...shown.querySelectorAll('ul[aria-label="What will happen"] > li')].map(
    (line) => line.textContent,
  );
const submit = (shown: HTMLElement) => findByText<HTMLButtonElement>(shown, "button", "Ship");
const chips = (header: HTMLElement) =>
  [...header.querySelectorAll('ul[aria-label="Pull requests"] > li')].map(
    (chip) => chip.textContent,
  );
const lazily = (check: () => void): Promise<void> =>
  vi.waitFor(() => {
    flush();
    check();
  });

beforeEach(() => {
  history.replaceState(null, "", "/");
  resetFakeEditors();
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
});

afterEach(() => {
  vi.useRealTimers();
  unmountAll();
  sessionStorage.clear();
});

describe("shipping from review", () => {
  it("offers to open pull requests once the session is in review with a changed worktree", async () => {
    const { header, emit } = await openSession();
    const button = action(header);
    expect(button.textContent).toBe("Open pull requests");
    expect(button.disabled).toBe(false);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.classList.contains("btn-primary")).toBe(true);

    // api.rs, "Shipping": only from review or shipping.
    for (const status of ["running", "waiting", "failed", "cancelled", "done"] as const) {
      emit({ type: "session_status", status, reason: null });
      expect([status, delivery(header)]).toEqual([status, null]);
    }
    emit({ type: "session_status", status: "review", reason: null });
    expect(action(header).textContent).toBe("Open pull requests");
  });

  it("offers nothing when no worktree changed, whatever the other repos hold", async () => {
    const { header } = await openSession({
      changes: [changed("docs", 2, 2, 2), changed("notes", 1, 1, 0), changed("api", 0, 0, 0)],
    });
    expect(delivery(header)).toBeNull();
  });

  it("reads Update once the session has pull requests, in review and while shipping", async () => {
    const seeded = await openSession({ pullRequests: [makePullRequest("api", 12)] });
    expect(action(seeded.header).textContent).toBe("Update pull requests");
    unmountAll();

    const shipping = await openSession({
      status: "shipping",
      pullRequests: [makePullRequest("api", 12)],
    });
    const button = action(shipping.header);
    expect(button.textContent).toBe("Update pull requests");
    // Review ends with the primary action; shipping again is the lesser one.
    expect(button.classList.contains("btn-primary")).toBe(false);
  });

  it("opens a form prefilled with the session's title, drafts on, and says what will happen", async () => {
    const { main, header } = await openSession();
    const shown = await openForm(header.parentElement ?? main);
    expect(action(header).getAttribute("aria-expanded")).toBe("true");

    const title = control(shown, "Pull request title");
    expect(title.value).toBe("Add a health check");
    expect(document.activeElement).toBe(title);
    const description = control<HTMLTextAreaElement>(shown, "Description");
    expect(description.tagName).toBe("TEXTAREA");
    expect(description.value).toBe("");
    expect(shown.textContent).toContain("(optional, Markdown)");
    expect(control(shown, "Open as drafts").checked).toBe(true);

    expect(planLines(shown)).toEqual([
      "api: 3 files, +40 −12 → new pull request",
      "web: 1 file, +5 −0 → new pull request",
    ]);
    expect(find(shown, '[data-part="skipped"]').textContent).toBe(
      "Skipped: docs (read only), notes (edited in place), tools (no changes)",
    );
    // Not a dry run: nothing says so.
    expect(shown.querySelector('[data-part="dry-run"]')).toBeNull();

    await click(findByText(shown, "button", "Cancel"));
    expect(form(main)).toBeNull();
    expect(action(header).getAttribute("aria-expanded")).toBe("false");
  });

  it("closes on Escape while nothing was typed, and keeps what was", async () => {
    const { main } = await openSession();
    const escape = (target: HTMLElement) => {
      target.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      flush();
    };
    escape(control(await openForm(main), "Pull request title"));
    expect(form(main)).toBeNull();

    const shown = await openForm(main);
    type(control<HTMLTextAreaElement>(shown, "Description"), "Closes #4");
    escape(control<HTMLTextAreaElement>(shown, "Description"));
    expect(form(main)).toBe(shown);
    expect(control<HTMLTextAreaElement>(shown, "Description").value).toBe("Closes #4");
  });

  it("says which pull requests are updated, and asks about drafts only when one is opened", async () => {
    const mixed = await openSession({
      pullRequests: [makePullRequest("api", 12, { state: "draft" })],
    });
    const some = await openForm(mixed.main);
    expect(planLines(some)).toEqual([
      "api: 3 files, +40 −12 → updates #12",
      "web: 1 file, +5 −0 → new pull request",
    ]);
    expect(control(some, "Open as drafts").checked).toBe(true);
    unmountAll();

    const all = await openSession({
      status: "shipping",
      pullRequests: [makePullRequest("api", 12), makePullRequest("web", 7)],
    });
    const none = await openForm(all.main);
    expect(planLines(none)).toEqual([
      "api: 3 files, +40 −12 → updates #12",
      "web: 1 file, +5 −0 → updates #7",
    ]);
    // Updating: a draft is something a pull request is opened as.
    expect(none.querySelector('input[type="checkbox"]')).toBeNull();
    await click(submit(none));
    expect(all.shipSession).toHaveBeenCalledWith("s1", { title: "Add a health check" });
  });

  it("sends the title and drafts by default", async () => {
    const { main, shipSession } = await openSession();
    await click(submit(await openForm(main)));
    expect(shipSession).toHaveBeenCalledTimes(1);
    expect(shipSession).toHaveBeenCalledWith("s1", { title: "Add a health check", draft: true });
  });

  it("sends what the reader edited: title, description, not drafts", async () => {
    const { main, shipSession } = await openSession();
    const shown = await openForm(main);
    type(control(shown, "Pull request title"), "  Health endpoint  ");
    type(control<HTMLTextAreaElement>(shown, "Description"), "Closes #4\n\n- adds `/health`\n");
    await click(control(shown, "Open as drafts"));
    expect(control(shown, "Open as drafts").checked).toBe(false);
    await click(submit(shown));
    expect(shipSession).toHaveBeenCalledWith("s1", {
      title: "Health endpoint",
      body: "Closes #4\n\n- adds `/health`",
      draft: false,
    });
  });

  it("leaves out a title the reader emptied, so the server uses the session's", async () => {
    const { main, shipSession } = await openSession();
    const shown = await openForm(main);
    type(control(shown, "Pull request title"), "   ");
    expect(control(shown, "Pull request title").placeholder).toBe("Add a health check");
    await click(submit(shown));
    expect(shipSession).toHaveBeenCalledWith("s1", { draft: true });
  });

  it("is disabled, with the reason, while GitHub cannot be used, and asks again on request", async () => {
    const read = vi
      .fn<Api["health"]>()
      .mockResolvedValueOnce({ ...health, github_problem: "the gh CLI is not signed in" })
      .mockResolvedValue(health);
    const { main, header } = await openSession({ routes: { health: read } });
    const button = action(header);
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("Open pull requests");
    expect(delivery(header)?.textContent).toContain(
      "GitHub is not available: the gh CLI is not signed in",
    );
    await click(button);
    expect(form(main)).toBeNull();

    // The reader signed in meanwhile.
    await click(findByText(header, "button", "Check GitHub again"));
    await act();
    expect(read).toHaveBeenCalledTimes(2);
    expect(action(header).disabled).toBe(false);
    expect(delivery(header)?.textContent).not.toContain("GitHub is not available");
  });

  it("works in dry run, and says that no pull request is really opened", async () => {
    const { main, header, shipSession } = await openSession({
      routes: {
        // A dry run never calls GitHub, so its being unusable is not in the way.
        health: () =>
          Promise.resolve({ ...health, dry_run: true, github_problem: "the gh CLI is missing" }),
      },
    });
    expect(action(header).disabled).toBe(false);
    const shown = await openForm(main);
    expect(find(shown, '[data-part="dry-run"]').textContent).toBe(
      "Dry run: no pull request is really opened",
    );
    await click(submit(shown));
    expect(shipSession).toHaveBeenCalledTimes(1);
  });

  it("shows nothing as opened until the events say so, and says it is working meanwhile", async () => {
    const answer = deferred();
    const shipSession = vi.fn<Api["shipSession"]>().mockReturnValue(answer.promise);
    const { main, header, emit } = await openSession({ routes: { shipSession } });
    const shown = await openForm(main);
    await click(submit(shown));

    // The request is on its way: the form says so and takes no second one.
    expect(submit(shown).textContent).toBe("Shipping…");
    expect(submit(shown).disabled).toBe(true);
    expect(control(shown, "Pull request title").disabled).toBe(true);
    await click(submit(shown));
    expect(shipSession).toHaveBeenCalledTimes(1);

    answer.resolve();
    await act();
    // Taken: the form closes, and the action waits for the session's events.
    expect(form(main)).toBeNull();
    const button = action(header);
    expect([button.textContent, button.disabled]).toEqual(["Opening pull requests…", true]);
    expect(chips(header)).toEqual([]);
    expect(header.textContent).toContain("Review");

    emit(opened("api", 12, { state: "draft" }));
    expect(chips(header)).toEqual(["api #12 Draft"]);
    expect([action(header).textContent, action(header).disabled]).toEqual([
      "Opening pull requests…",
      true,
    ]);
    emit({ type: "session_status", status: "shipping", reason: null });
    emit(opened("web", 7, { state: "draft" }));
    expect(chips(header)).toEqual(["api #12 Draft", "web #7 Draft"]);
    expect([action(header).textContent, action(header).disabled]).toEqual([
      "Update pull requests",
      false,
    ]);
    expect(header.textContent).toContain("Shipping");
    // Each one is said once in the transcript, leading to it.
    const rows = [...main.querySelectorAll("[data-pull-request-row]")].map(
      (row) => row.textContent,
    );
    expect(rows).toEqual(["Pull request #12 opened for api", "Pull request #7 opened for web"]);
  });

  it("keeps the form open with the server's message when the request is refused", async () => {
    const shipSession = vi
      .fn<Api["shipSession"]>()
      .mockRejectedValueOnce(conflict("the session is not in review"))
      .mockResolvedValue(undefined);
    const { main, header } = await openSession({ routes: { shipSession } });
    const shown = await openForm(main);
    type(control(shown, "Pull request title"), "Health endpoint");
    await click(submit(shown));

    expect(find(shown, '[role="alert"]').textContent).toBe("the session is not in review");
    // Nothing was lost, and nothing claims to be shipping.
    expect(control(shown, "Pull request title").value).toBe("Health endpoint");
    expect(submit(shown).disabled).toBe(false);
    expect(action(header).textContent).toBe("Open pull requests");

    await click(submit(shown));
    expect(shipSession).toHaveBeenCalledTimes(2);
    expect(form(main)).toBeNull();
  });

  it("counts the repos that failed in the last ship, with the way to the transcript", async () => {
    const { main, header, emit } = await openSession();
    await click(submit(await openForm(main)));
    emit({ type: "error", message: "web: the push was rejected" });
    emit(opened("api", 12));
    emit({ type: "session_status", status: "shipping", reason: null });

    const note = find(header, '[data-part="ship-failures"]');
    expect(note.textContent).toContain("1 repo failed in the last attempt.");
    expect(findByText<HTMLAnchorElement>(note, "a", "transcript").getAttribute("href")).toBe(
      "/s/s1",
    );
    expect(find(main, '[role="status"][aria-live="polite"]').textContent).toContain(
      "1 repo could not be shipped. The transcript says why.",
    );
    // The error itself is in the transcript, as every error is.
    expect(find(main, '[role="log"]').textContent).toContain("web: the push was rejected");
    expect(action(header).textContent).toBe("Update pull requests");

    // Trying again starts the count over.
    await click(submit(await openForm(main)));
    expect(header.querySelector('[data-part="ship-failures"]')).toBeNull();
    expect(action(header).textContent).toBe("Updating pull requests…");
  });

  it("closes the form when the session leaves review, and does not bring it back by itself", async () => {
    const { main, header, emit } = await openSession();
    await openForm(main);
    emit({ type: "session_status", status: "running", reason: null });
    expect(form(main)).toBeNull();
    emit({ type: "session_status", status: "review", reason: null });
    expect(form(main)).toBeNull();
    expect(action(header).getAttribute("aria-expanded")).toBe("false");
  });

  it("is offered in the Changes panel too on a wide screen, and opens the same form", async () => {
    const { main, header } = await openSession({ path: "/s/s1/changes" });
    await lazily(() => expect(main.querySelector('[aria-label="Changed files"]')).not.toBeNull());
    await act();
    const panel = find(main, 'section[aria-label="Changed files"]');
    const inPanel = action(panel);
    expect(inPanel.textContent).toBe("Open pull requests");
    expect(main.querySelectorAll('[aria-label="Delivery"]')).toHaveLength(2);

    await click(inPanel);
    expect(main.querySelectorAll('form[aria-label="Ship the session"]')).toHaveLength(1);
    expect(action(header).getAttribute("aria-expanded")).toBe("true");
  });

  it("is offered once on a phone, in the header, in reach from every panel", async () => {
    setViewportWidth(PHONE);
    const { main, header } = await openSession({ path: "/s/s1/changes" });
    await lazily(() => expect(main.querySelector('[aria-label="Changed files"]')).not.toBeNull());
    await act();
    expect(main.querySelectorAll('[aria-label="Delivery"]')).toHaveLength(1);
    expect(action(header).textContent).toBe("Open pull requests");
  });
});

describe("pull requests on the session page", () => {
  const shipped = [
    makePullRequest("api", 12, { checks: "failing", review: "changes_requested" }),
    makePullRequest("web", 7, { state: "merged", checks: "passing" }),
  ];

  it("shows them as chips in the header, each with its state, checks and review in words", async () => {
    const { header, emit } = await openSession({ status: "shipping", pullRequests: shipped });
    expect(chips(header)).toEqual([
      "api #12 Open Checks failing Changes requested",
      "web #7 Merged Checks passing",
    ]);
    const link = find<HTMLAnchorElement>(header, 'a[data-pull-request="12"]');
    expect([link.getAttribute("href"), link.target, link.rel]).toEqual([
      "https://github.com/acme/api/pull/12",
      "_blank",
      "noopener noreferrer",
    ]);
    expect(link.getAttribute("aria-label")).toBe("Pull request #12 for api (opens in a new tab)");

    emit(updated("api", 12, { checks: "passing", review: "approved" }));
    expect(chips(header)[0]).toBe("api #12 Open Checks passing Approved");
  });

  it("writes the agent a message about what failed, and leaves the sending to the reader", async () => {
    const sendMessage = vi.fn<Api["sendMessage"]>().mockResolvedValue(undefined);
    const { main } = await openSession({
      status: "shipping",
      pullRequests: shipped,
      path: "/s/s1/changes",
      routes: { sendMessage },
    });
    await lazily(() => expect(main.textContent).toContain("Send back to the agent"));
    const box = find<HTMLTextAreaElement>(main, "textarea");
    expect(box.disabled).toBe(false);
    expect(box.value).toBe("");

    await click(findByText(main, "button", "Send back to the agent"));
    await act();
    expect(box.value).toBe(
      [
        "This pull request needs another pass:",
        "- api: checks are failing and changes were requested on https://github.com/acme/api/pull/12",
        "Please look at what failed and fix it.",
      ].join("\n"),
    );
    expect(document.activeElement).toBe(box);
    // Three lines written for the reader: the box shows more of them than its usual two.
    expect(box.getAttribute("rows")).toBe("5");
    expect(sendMessage).not.toHaveBeenCalled();

    // Editable, and sent like any message: it starts a turn.
    type(box, `${box.value}\nKeep the public API as it is.`);
    await click(find<HTMLButtonElement>(main, 'button[type="submit"]'));
    expect(sendMessage).toHaveBeenCalledTimes(1);
    expect(sendMessage.mock.calls[0][1].text).toContain("Keep the public API as it is.");
  });

  it("on a phone, brings the message box into view before filling it", async () => {
    setViewportWidth(PHONE);
    const { main } = await openSession({
      status: "shipping",
      pullRequests: shipped,
      path: "/s/s1/changes",
    });
    await lazily(() => expect(main.textContent).toContain("Send back to the agent"));
    await click(findByText(main, "button", "Send back to the agent"));
    await act();
    expect(location.pathname).toBe("/s/s1");
    const box = find<HTMLTextAreaElement>(main, "textarea");
    expect(box.value).toContain("- api: checks are failing and changes were requested");
    expect(document.activeElement).toBe(box);
  });
});

describe("the statuses after review", () => {
  it("keeps a shipping session open: it takes a message, can be stopped, not interrupted", async () => {
    const sendMessage = vi.fn<Api["sendMessage"]>().mockResolvedValue(undefined);
    const stopSession = vi.fn<Api["stopSession"]>().mockResolvedValue(undefined);
    const { main, header } = await openSession({
      status: "shipping",
      pullRequests: [makePullRequest("api", 12)],
      routes: { sendMessage, stopSession },
    });
    expect(header.textContent).toContain("Shipping");
    const box = find<HTMLTextAreaElement>(main, "textarea");
    expect([box.disabled, box.placeholder]).toEqual([false, "Message the agent"]);
    type(box, "Rebase on main, please");
    await click(find<HTMLButtonElement>(main, 'button[type="submit"]'));
    expect(sendMessage).toHaveBeenCalledWith("s1", { text: "Rebase on main, please" });

    expect(offersButton(header, "Interrupt")).toBe(false);
    const stop = findByText<HTMLButtonElement>(header, "button", "Stop");
    expect(stop.disabled).toBe(false);
    await click(stop);
    expect(stopSession).toHaveBeenCalledWith("s1");
    expect(main.querySelector('[data-part="done-note"]')).toBeNull();
  });

  it("leads the tab title with a pull request that needs the reader, while shipping only", async () => {
    const { emit } = await openSession({
      status: "shipping",
      pullRequests: [makePullRequest("api", 12, { checks: "pending" })],
    });
    expect(document.title).toBe("Add a health check · Calin");
    emit(updated("api", 12, { checks: "failing" }));
    expect(document.title).toBe("Pull request needs you · Add a health check · Calin");
    emit(updated("api", 12, { checks: "passing" }));
    expect(document.title).toBe("Add a health check · Calin");
    emit(updated("api", 12, { state: "merged", checks: "failing" }));
    emit({ type: "session_status", status: "done", reason: null });
    expect(document.title).toBe("Add a health check · Calin");
  });

  it("makes a done session read-only, and says so in one line", async () => {
    const { main, header, emit } = await openSession({
      status: "shipping",
      pullRequests: [makePullRequest("api", 12)],
    });
    emit(updated("api", 12, { state: "merged" }));
    emit({ type: "session_status", status: "done", reason: null });

    expect(header.textContent).toContain("Done");
    expect(find(main, '[data-part="done-note"]').textContent).toBe(
      "Done: every pull request is merged or closed. This session is now read-only.",
    );
    const box = find<HTMLTextAreaElement>(main, "textarea");
    expect([box.disabled, box.placeholder]).toEqual([
      true,
      "This session is done: it can be read, not continued",
    ]);
    expect(find<HTMLButtonElement>(main, 'button[type="submit"]').disabled).toBe(true);
    expect(offersButton(header, "Interrupt")).toBe(false);
    expect(offersButton(header, "Stop")).toBe(false);
    expect(findByText<HTMLButtonElement>(header, "button", "Attach repo").disabled).toBe(true);
    // Nothing more to ship; the pull requests stay on show.
    expect(delivery(header)).toBeNull();
    expect(chips(header)).toEqual(["api #12 Merged"]);
    expect(find(main, '[role="log"]').textContent).toContain("Pull request #12 merged for api");
  });

  it("says a done session without pull requests is read-only, with no claim about any", async () => {
    const { main } = await openSession({ status: "done" });
    expect(find(main, '[data-part="done-note"]').textContent).toBe(
      "Done: this session is now read-only.",
    );
  });

  it("marks the sessions that are shipping in the list, and only those", async () => {
    const { host, socket } = await openSession({ status: "shipping" });
    for (const [id, status] of [
      ["s1", "shipping"],
      ["s2", "review"],
      ["s3", "done"],
    ] as const) {
      socket.emit("sessions", {
        op: "session_upserted",
        session: makeSession(id, 1000, { title: `Session ${id}`, status }),
      });
    }
    socket.emit("sessions", { op: "subscribed", topic: "sessions", head_seq: 0 });
    flush();
    const rows = [...find(host, 'nav[aria-label="Sessions"]').querySelectorAll("li")].map((row) => [
      row.textContent.includes("Session s1")
        ? "s1"
        : row.textContent.includes("Session s2")
          ? "s2"
          : "s3",
      row.querySelector('[data-part="pull-requests"]')?.textContent ?? null,
    ]);
    expect(rows.toSorted(([a], [b]) => String(a).localeCompare(String(b)))).toEqual([
      ["s1", "Pull requests open"],
      ["s2", null],
      ["s3", null],
    ]);
    expect(findByText(host, "nav li", "Session s1").textContent).toContain("Shipping");
    expect(findByText(host, "nav li", "Session s3").textContent).toContain("Done");
  });
});
