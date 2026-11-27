import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Health } from "./gen/Health";
import { createAppState } from "./state/app";
import type { Item } from "./gen/Item";
import type { PermissionRequest } from "./gen/PermissionRequest";
import type { Repo } from "./gen/Repo";
import type { RepoChanges } from "./gen/RepoChanges";
import type { Settings } from "./gen/Settings";
import type { SessionRepo } from "./gen/SessionRepo";
import type { SessionStatus } from "./gen/SessionStatus";
import type { OpenTerminal } from "./api/terminal";
import { editorFor, editors, resetFakeEditors } from "./test/fakeEditor";
import { fakeTerminals, resetFakeTerminals, views } from "./test/fakeTerminal";
import {
  conflict,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeMissingRepo,
  makeRepo,
  makeSession,
  makeSessionRepo,
  notFound,
  settle,
  unreachable,
} from "./test/fakes";
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
} from "./test/render";

// The lazy panels load these in place of CodeMirror and xterm.js (see src/test/).
vi.mock("./panels/editor/codemirror", async () => ({
  default: (await import("./test/fakeEditor")).fakeEditorModule,
}));
vi.mock("./panels/terminal/xterm", async () => ({
  default: (await import("./test/fakeTerminal")).fakeTerminalModule,
}));

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock", "claude"],
};

/** Mounts the whole app at `path` over fakes, with the socket already open. */
async function start(path: string, routes: Partial<Api> = {}, openTerminal?: OpenTerminal) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    // What the Changes panel asks for when it opens.
    getChanges: () => Promise.resolve([]),
    ...routes,
  });
  const state = createAppState({ api, socket, schedule: immediately, openTerminal });
  const host = mount(() => <App state={state} />);
  await settle();
  flush();
  const listSessions = (...sessions: ReturnType<typeof makeSession>[]) => {
    for (const session of sessions) socket.emit("sessions", { op: "session_upserted", session });
    socket.emit("sessions", { op: "subscribed", topic: "sessions", head_seq: 0 });
    flush();
  };
  return { host, socket, listSessions };
}

/** Opens session `s1` and returns what a test needs to drive it. */
async function openSession(
  path: string,
  routes: Partial<Api> = {},
  status: SessionStatus = "running",
  repos: SessionRepo[] = [],
) {
  const session = makeSession("s1", 1000, { title: "Add a health check", status });
  const getSession = vi.fn<Api["getSession"]>().mockResolvedValue(makeDetail(session, repos));
  const terminals = fakeTerminals();
  const app = await start(path, { getSession, ...routes }, terminals.open);
  let seq = 0;
  const emit = (body: Parameters<typeof makeEvent>[1], agentId: string | null = "a1") => {
    seq += 1;
    app.socket.emit("session:s1", {
      op: "event",
      topic: "session:s1",
      event: makeEvent(seq, body, agentId),
    });
    flush();
  };
  /** The replay is over: from here the screen shows live state. */
  const caughtUp = () => {
    app.socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: seq });
    flush();
  };
  const main = find(app.host, "main");
  const tab = (label: string) => findByText<HTMLButtonElement>(main, '[role="tab"]', label);
  return { ...app, main, emit, caughtUp, getSession, tab, shells: terminals.shells };
}

const permissionRequest = (id: string, title: string): PermissionRequest => ({
  id,
  tool_name: "Bash",
  tool_kind: "execute",
  title,
  input: { command: title },
  item_id: null,
});

/** A lazy panel arrives a moment after it is first shown. */
const shown = (root: ParentNode, text: string): Promise<void> =>
  vi.waitFor(() => {
    flush();
    expect(root.textContent).toContain(text);
  });

async function act(): Promise<void> {
  await settle();
  flush();
}

beforeEach(() => {
  history.replaceState(null, "", "/");
  resetFakeEditors();
  resetFakeTerminals();
});

afterEach(() => {
  unmountAll();
  sessionStorage.clear();
  vi.unstubAllGlobals();
});

describe("App", () => {
  it("says so when the server cannot be reached, and offers a retry", async () => {
    const read = vi
      .fn<Api["health"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(health);
    const { host, socket } = await start("/", { health: read });
    socket.setState("reconnecting");
    flush();

    expect(find(host, '[role="alert"]').textContent).toContain("Cannot reach the server");
    expect(host.textContent).toContain("Reconnecting…");
    expect(host.querySelector("textarea")).toBeNull();

    findByText(host, "button", "Retry now").click();
    await act();
    socket.setState("open");
    await act();

    expect(socket.reconnects).toBe(1);
    expect(host.textContent).not.toContain("Cannot reach the server");
    expect(host.textContent).not.toContain("Reconnecting…");
    expect(host.querySelector("textarea")).not.toBeNull();
  });

  it("shows the connection indicator only while the socket is not open", async () => {
    const { host, socket } = await start("/");
    const indicator = find(host, '[role="status"]');
    expect(indicator.textContent).toBe("Connecting…");

    socket.setState("open");
    flush();
    expect(indicator.textContent).toBe("");

    socket.setState("reconnecting");
    flush();
    expect(indicator.textContent).toBe("Reconnecting…");
  });

  it("lists sessions with title, status, runner and age", async () => {
    vi.useFakeTimers({ now: 10 * 60_000, toFake: ["Date"] });
    try {
      const { host, listSessions } = await start("/");
      expect(host.textContent).toContain("Loading sessions…");

      listSessions(
        makeSession("a", 0, { title: "Fix the flaky test", status: "running", runner: "claude" }),
        makeSession("b", 7 * 60_000, { title: "Write the changelog", status: "review" }),
      );

      const links = [...host.querySelectorAll<HTMLAnchorElement>("nav a")];
      expect(links.map((link) => link.getAttribute("href"))).toEqual(["/s/b", "/s/a"]);
      expect(links[0].textContent).toContain("Write the changelog");
      expect(links[0].textContent).toContain("Review");
      expect(links[0].textContent).toContain("mock");
      expect(links[0].textContent).toContain("3 min ago");
      expect(links[1].textContent).toContain("Fix the flaky test");
      expect(links[1].textContent).toContain("Running");
      expect(links[1].textContent).toContain("claude");
      expect(links[1].textContent).toContain("10 min ago");
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows an empty state when there are no sessions", async () => {
    const { host, listSessions } = await start("/");
    listSessions();
    expect(host.textContent).toContain("No sessions yet");
    expect(host.textContent).not.toContain("Loading sessions…");
  });

  it("shows the dry-run banner only when the server says so", async () => {
    const real = await start("/");
    expect(real.host.textContent).not.toContain("Dry run");
    unmountAll();

    const dry = await start("/", { health: () => Promise.resolve({ ...health, dry_run: true }) });
    expect(dry.host.textContent).toContain("Dry run: agents are simulated");
  });

  it("offers the server's runners, default first", async () => {
    const { host } = await start("/");
    const select = find<HTMLSelectElement>(host, "select");
    expect([...select.options].map((option) => option.value)).toEqual(["mock", "claude"]);
    expect(select.value).toBe("mock");
  });

  it("starts a session with Ctrl+Enter and opens it", async () => {
    const created = makeSession("new", 5000, { title: "Tidy the README", status: "queued" });
    const createSession = vi.fn<Api["createSession"]>().mockResolvedValue(makeDetail(created));
    const getSession = vi.fn<Api["getSession"]>().mockResolvedValue(makeDetail(created));
    const { host } = await start("/", { createSession, getSession });
    const prompt = find<HTMLTextAreaElement>(host, "textarea");
    const runner = find<HTMLSelectElement>(host, "select");

    runner.value = "claude";
    runner.dispatchEvent(new Event("change", { bubbles: true }));
    type(prompt, "  Tidy the README\nand keep it short  ");
    prompt.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }),
    );
    await act();

    expect(createSession).toHaveBeenCalledWith({
      prompt: "Tidy the README\nand keep it short",
      permission_mode: "ask",
      repos: [],
      runner: "claude",
    });
    expect(location.pathname).toBe("/s/new");
    expect(getSession).toHaveBeenCalledWith("new");
    expect(find(host, "main h1").textContent).toBe("Tidy the README");
  });

  it("does not start a session from a blank prompt, and reports a refusal", async () => {
    const createSession = vi
      .fn<Api["createSession"]>()
      .mockRejectedValue(new Error("runner is not available"));
    const { host } = await start("/", { createSession });
    const prompt = find<HTMLTextAreaElement>(host, "textarea");
    const submit = findByText<HTMLButtonElement>(host, "button", "Start");

    type(prompt, "   \n ");
    submit.click();
    await act();
    expect(createSession).not.toHaveBeenCalled();

    type(prompt, "Do it");
    submit.click();
    await act();
    expect(createSession).toHaveBeenCalledTimes(1);
    expect(find(host, 'form [role="alert"]').textContent).toBe("runner is not available");
    expect(location.pathname).toBe("/");
    // The prompt survives the failure, ready to be sent again.
    expect(prompt.value).toBe("Do it");
  });

  it("shows a live session: transcript, plan, usage, and controls that follow its status", async () => {
    const session = makeSession("s1", 1000, { title: "Add a health check", status: "running" });
    const stopSession = vi.fn<Api["stopSession"]>().mockResolvedValue();
    const sendMessage = vi.fn<Api["sendMessage"]>().mockResolvedValue();
    const { host, socket } = await start("/s/s1", {
      getSession: () => Promise.resolve(makeDetail(session)),
      stopSession,
      sendMessage,
    });
    const main = find(host, "main");
    const stop = findByText<HTMLButtonElement>(main, "button", "Stop");
    const send = findByText<HTMLButtonElement>(main, "button", "Send");
    const box = find<HTMLTextAreaElement>(main, "textarea");
    const emit = (seq: number, body: Parameters<typeof makeEvent>[1]) =>
      socket.emit("session:s1", {
        op: "event",
        topic: "session:s1",
        event: makeEvent(seq, body, "a1"),
      });

    expect(find(main, "h1").textContent).toBe("Add a health check");
    expect(main.textContent).toContain("Loading transcript…");
    expect(main.textContent).not.toContain("$");

    emit(1, { type: "session_status", status: "running", reason: null });
    emit(2, { type: "user_message", text: "Add a health check" });
    emit(3, {
      type: "item_started",
      item: { id: "m1", status: "running", content: { kind: "text", text: "" } },
    });
    emit(4, { type: "item_delta", item_id: "m1", delta: { kind: "text", text: "On it." } });
    emit(5, {
      type: "plan_updated",
      entries: [
        { content: "Read the router", status: "completed" },
        { content: "Add the route", status: "in_progress" },
      ],
    });
    emit(6, { type: "usage", cost_usd: 0.0421, tokens_in: 12_345, tokens_out: 678 });
    socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: 6 });
    flush();

    expect(find(main, '[role="log"]').textContent).toContain("Add a health check");
    expect(find(main, '[role="log"]').textContent).toContain("On it.");
    expect(main.textContent).toContain("1 of 2 done");
    expect(main.textContent).toContain("Add the route");
    expect(find(main, "header").textContent).toContain("Running");
    expect(find(main, "header").textContent).toContain("$0.0421 · 12.3k in · 678 out");
    expect(stop.disabled).toBe(false);
    expect(box.disabled).toBe(false);

    type(box, "Use /healthz");
    send.click();
    await act();
    expect(sendMessage).toHaveBeenCalledWith("s1", { text: "Use /healthz" });
    expect(box.value).toBe("");

    stop.click();
    await act();
    expect(stopSession).toHaveBeenCalledWith("s1");

    emit(7, { type: "session_status", status: "cancelled", reason: "stopped by the user" });
    flush();
    expect(find(main, "header").textContent).toContain("Cancelled");
    expect(stop.disabled).toBe(true);
    // A stopped session can be picked up again: the next message restarts the agent.
    expect(box.disabled).toBe(false);
    expect(send.textContent).toBe("Continue");

    type(box, "Try once more");
    send.click();
    await act();
    expect(sendMessage).toHaveBeenLastCalledWith("s1", { text: "Try once more" });
  });

  it("announces status and whole agent messages, never partial ones", async () => {
    const session = makeSession("s1", 1000, { status: "running" });
    const { host, socket } = await start("/s/s1", {
      getSession: () => Promise.resolve(makeDetail(session)),
    });
    const announcer = find(host, 'main [role="status"]');
    const emit = (seq: number, body: Parameters<typeof makeEvent>[1]) =>
      socket.emit("session:s1", { op: "event", topic: "session:s1", event: makeEvent(seq, body) });

    emit(1, { type: "session_status", status: "running", reason: null });
    emit(2, {
      type: "item_started",
      item: { id: "m1", status: "running", content: { kind: "text", text: "" } },
    });
    emit(3, { type: "item_delta", item_id: "m1", delta: { kind: "text", text: "Half a sen" } });
    socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: 3 });
    flush();
    expect(announcer.textContent).toBe("Session status: Running");

    emit(4, {
      type: "item_completed",
      item: {
        id: "m1",
        status: "completed",
        content: { kind: "text", text: "Half a sentence, now whole." },
      },
    });
    flush();
    expect(announcer.textContent).toBe("Session status: RunningHalf a sentence, now whole.");
  });

  it("enables the controls for each status as the contract says", async () => {
    const { host, socket } = await start("/s/s1", {
      getSession: () => Promise.resolve(makeDetail(makeSession("s1"))),
    });
    const main = find(host, "main");
    const interrupt = findByText<HTMLButtonElement>(main, "button", "Interrupt");
    const stop = findByText<HTMLButtonElement>(main, "button", "Stop");
    const box = find<HTMLTextAreaElement>(main, "textarea");
    const send = find<HTMLButtonElement>(main, 'button[type="submit"]');
    socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: 0 });

    // The "Semantics" section of calin-proto/src/api.rs, one row per status.
    const expected = {
      queued: { interrupt: false, stop: true, message: false, label: "Send" },
      preparing: { interrupt: false, stop: true, message: false, label: "Send" },
      running: { interrupt: true, stop: true, message: true, label: "Send" },
      waiting: { interrupt: true, stop: true, message: true, label: "Send" },
      review: { interrupt: false, stop: true, message: true, label: "Send" },
      shipping: { interrupt: false, stop: false, message: false, label: "Send" },
      done: { interrupt: false, stop: false, message: false, label: "Send" },
      failed: { interrupt: false, stop: false, message: true, label: "Continue" },
      cancelled: { interrupt: false, stop: false, message: true, label: "Continue" },
    } as const;
    let seq = 0;
    for (const [status, can] of Object.entries(expected)) {
      seq += 1;
      socket.emit("session:s1", {
        op: "event",
        topic: "session:s1",
        event: makeEvent(seq, {
          type: "session_status",
          status: status as keyof typeof expected,
          reason: null,
        }),
      });
      flush();
      expect({
        status,
        interrupt: !interrupt.disabled,
        stop: !stop.disabled,
        message: !box.disabled && !send.disabled,
        label: send.textContent,
      }).toEqual({ status, ...can });
    }
  });

  it("shows a not-found state with a way back for an unknown session", async () => {
    const { host } = await start("/s/missing", { getSession: () => Promise.reject(notFound()) });
    const main = find(host, "main");
    expect(main.textContent).toContain("Session not found");
    expect(findByText<HTMLAnchorElement>(main, "a", "Back to sessions").getAttribute("href")).toBe(
      "/",
    );
    expect(main.querySelector("textarea")).toBeNull();
  });

  it("says a session could not load when the server is unreachable", async () => {
    const { host, socket } = await start("/s/s1", {
      getSession: () => Promise.reject(unreachable()),
    });
    socket.setState("open");
    flush();
    const main = find(host, "main");
    expect(main.textContent).toContain("Cannot reach the server");
    expect(findByText(main, "button", "Try again")).toBeDefined();
  });

  it("shows a not-found page for an unknown address", async () => {
    const { host } = await start("/nowhere");
    expect(find(host, "main").textContent).toContain("Page not found");
  });

  it("remounts the session screen when the address moves to another session", async () => {
    const getSession = vi.fn<Api["getSession"]>((id) =>
      Promise.resolve(makeDetail(makeSession(id, 1000, { title: `Title of ${id}` }))),
    );
    const { host, socket, listSessions } = await start("/s/one", { getSession });
    listSessions(
      makeSession("one", 1000, { title: "Title of one" }),
      makeSession("two", 2000, { title: "Title of two" }),
    );
    expect(find(host, "main h1").textContent).toBe("Title of one");
    expect(socket.subscriberCount("session:one")).toBe(1);

    findByText<HTMLAnchorElement>(host, "nav a", "Title of two").click();
    await act();

    expect(location.pathname).toBe("/s/two");
    expect(find(host, "main h1").textContent).toBe("Title of two");
    expect(socket.subscriberCount("session:one")).toBe(0);
    expect(socket.subscriberCount("session:two")).toBe(1);
  });

  it("offers the registered repos in the composer and attaches the chosen ones", async () => {
    const created = makeSession("new", 5000, { status: "queued" });
    const createSession = vi.fn<Api["createSession"]>().mockResolvedValue(makeDetail(created));
    const { host } = await start("/", {
      listRepos: () => Promise.resolve([makeRepo("web"), makeRepo("api")]),
      createSession,
      getSession: () => Promise.resolve(makeDetail(created)),
    });
    const main = find(host, "main");

    await click(control(main, "/code/web"));
    await click(control(main, "/code/api"));
    await click(findByText(main, "button", "Make primary"));
    type(find<HTMLTextAreaElement>(main, "textarea"), "Rename the endpoint");
    await click(findByText(main, "button", "Start"));

    expect(createSession).toHaveBeenCalledWith({
      prompt: "Rename the endpoint",
      permission_mode: "ask",
      repos: [
        { repo_id: "api", mode: "write" },
        { repo_id: "web", mode: "write" },
      ],
      runner: "mock",
    });
    expect(location.pathname).toBe("/s/new");
  });

  it("registers a repo from the composer and shows the server's refusal", async () => {
    const registerRepo = vi
      .fn<Api["registerRepo"]>()
      .mockRejectedValueOnce(conflict("a repo named api is already registered"))
      .mockResolvedValue(makeRepo("api2", { path: "/code/api" }));
    const { host } = await start("/", { registerRepo });
    const main = find(host, "main");
    expect(main.textContent).toContain("No repo is registered yet");

    await click(findByText(main, "button", "Register a repo"));
    const form = find<HTMLFormElement>(main, 'form[aria-label="Register a repo"]');
    type(control(form, "Path on the server"), "/code/api");
    await click(findByText(form, "button", "Register"));
    expect(find(form, '[role="alert"]').textContent).toBe("a repo named api is already registered");

    type(control(form, "Name"), "api2");
    await click(findByText(form, "button", "Register"));
    expect(registerRepo).toHaveBeenLastCalledWith({ path: "/code/api", name: "api2" });
    expect(main.querySelector('form[aria-label="Register a repo"]')).toBeNull();
    expect(control(main, "/code/api").checked).toBe(true);
  });

  it("shows the permission mode and the repo set in the header", async () => {
    const session = makeSession("s1", 1000, { status: "running", permission_mode: "accept_edits" });
    const repos = [
      makeSessionRepo("api", { primary: true, branch: "calin/health" }),
      makeSessionRepo("docs", { mode: "live" }),
    ];
    const { main, emit, caughtUp } = await openSession("/s/s1", {
      getSession: () => Promise.resolve(makeDetail(session, repos)),
    });
    const header = find(main, "header");
    const toggle = find<HTMLButtonElement>(header, "button[aria-controls]");
    const details = find(header, `[id="${toggle.getAttribute("aria-controls")}"]`);

    // One tap on a phone: the summary names the mode and counts the repos.
    expect(toggle.textContent).toContain("Accept edits · 2 repos");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");

    expect(details.textContent).toContain("Permissions: Accept edits");
    const chips = [...details.querySelectorAll("li")].map((chip) => chip.textContent);
    expect(chips).toEqual(["apiprimarycalin/health", "docsedit in place"]);

    // A repo attached later joins the set, and a branch that becomes known is shown.
    emit({ type: "repo_attached", repo: makeSessionRepo("web", { branch: "calin/health" }) }, null);
    caughtUp();
    expect([...details.querySelectorAll("li")].map((chip) => chip.textContent)).toEqual([
      "apiprimarycalin/health",
      "docsedit in place",
      "webcalin/health",
    ]);
    expect(toggle.textContent).toContain("3 repos");
  });

  it("interrupts a running session, and shows the server's refusal inline", async () => {
    const interruptSession = vi
      .fn<Api["interruptSession"]>()
      .mockRejectedValueOnce(conflict("session is not running"))
      .mockResolvedValue();
    const { main, emit, caughtUp } = await openSession("/s/s1", { interruptSession });
    const interrupt = findByText<HTMLButtonElement>(main, "button", "Interrupt");
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({
      type: "item_started",
      item: {
        id: "t1",
        status: "running",
        content: {
          kind: "tool",
          name: "Bash",
          tool_kind: "execute",
          title: "sleep 600",
          input: { command: "sleep 600" },
          output: null,
          locations: [],
        },
      },
    });
    caughtUp();

    await click(interrupt);
    expect(find(main, '[role="alert"]').textContent).toBe(
      "Could not interrupt the session: session is not running",
    );
    // Nothing is assumed: the session is still running on screen.
    expect(find(main, "header").textContent).toContain("Running");
    expect(interrupt.disabled).toBe(false);

    await click(interrupt);
    expect(interruptSession).toHaveBeenCalledTimes(2);
    expect(main.querySelector('[role="alert"]')).toBeNull();
    expect(find(main, "header").textContent).toContain("Running");
    expect(find(main, '[role="log"] summary').textContent).toContain("Running");

    // The status moves when the event says so, and the open tool call stops spinning.
    emit({ type: "session_status", status: "review", reason: "interrupted" }, null);
    expect(find(main, "header").textContent).toContain("Review");
    expect(interrupt.disabled).toBe(true);
    expect(find(main, '[role="log"] summary').textContent).toContain("Interrupted");
  });

  it("disables Interrupt and Stop while their request is in flight", async () => {
    let release = (): void => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const { main, emit, caughtUp } = await openSession("/s/s1", {
      interruptSession: () => held,
      stopSession: () => held,
    });
    emit({ type: "session_status", status: "running", reason: null }, null);
    caughtUp();
    const interrupt = findByText<HTMLButtonElement>(main, "button", "Interrupt");
    const stop = findByText<HTMLButtonElement>(main, "button", "Stop");

    await click(interrupt);
    await click(stop);
    expect([interrupt.disabled, interrupt.textContent]).toEqual([true, "Interrupting…"]);
    expect([stop.disabled, stop.textContent]).toEqual([true, "Stopping…"]);

    release();
    await act();
    expect([interrupt.disabled, interrupt.textContent]).toEqual([false, "Interrupt"]);
    expect([stop.disabled, stop.textContent]).toEqual([false, "Stop"]);
  });

  it("makes a waiting session visible: badge, tab title, pinned request, announcement", async () => {
    const resolvePermission = vi.fn<Api["resolvePermission"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await openSession("/s/s1", { resolvePermission });
    emit({ type: "session_status", status: "running", reason: null }, null);
    caughtUp();
    expect(document.title).toBe("Add a health check · Calin");
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();

    emit({ type: "permission_requested", request: permissionRequest("p1", "cargo test") });
    emit({ type: "session_status", status: "waiting", reason: null }, null);

    expect(document.title).toBe("Waiting for you · Add a health check · Calin");
    expect(find(main, "header").textContent).toContain("Waiting");
    expect(find(main, "header").textContent).toContain("1 request needs you");
    const bar = find(main, 'section[aria-label="Permission request"]');
    expect(bar.textContent).toContain("cargo test");
    expect(find(main, '[role="log"]').textContent).toContain("Waiting for you");
    const announcements = [...main.querySelectorAll('[role="status"]')].map(
      (region) => region.textContent,
    );
    expect(announcements).toContain("Permission requested: execute, cargo test.");

    await click(findByText(bar, "button", "Allow"));
    expect(resolvePermission).toHaveBeenCalledWith("s1", "p1", { decision: "allow" });
    // Not optimistic: the request stays until the session's own events answer it.
    expect(main.querySelector('section[aria-label="Permission request"]')).not.toBeNull();
    expect(find(main, '[role="log"]').textContent).toContain("Waiting for you");

    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "allow",
      by: "user",
      message: null,
    });
    emit({ type: "session_status", status: "running", reason: null }, null);
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
    expect(find(main, '[role="log"]').textContent).toContain("Allowed by you");
    expect(document.title).toBe("Add a health check · Calin");
    expect(find(main, "header").textContent).not.toContain("needs you");
  });

  it("denies a request with a reason, and records the denial in the transcript", async () => {
    const resolvePermission = vi.fn<Api["resolvePermission"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await openSession("/s/s1", { resolvePermission });
    emit({ type: "permission_requested", request: permissionRequest("p1", "git push --force") });
    emit({ type: "session_status", status: "waiting", reason: null }, null);
    caughtUp();
    const bar = find(main, 'section[aria-label="Permission request"]');

    await click(findByText(bar, "button", "Deny"));
    type(control(bar, "Reason for the agent"), "Never force-push.");
    await click(findByText(bar, "button", "Confirm deny"));
    expect(resolvePermission).toHaveBeenCalledWith("s1", "p1", {
      decision: "deny",
      message: "Never force-push.",
    });

    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "deny",
      by: "user",
      message: "Never force-push.",
    });
    const log = find(main, '[role="log"]').textContent;
    expect(log).toContain("Denied by you");
    expect(log).toContain("Never force-push.");
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
  });

  it("does not show requests from the middle of a replay, only those still open at its end", async () => {
    const { main, emit, caughtUp } = await openSession("/s/s1");
    emit({ type: "permission_requested", request: permissionRequest("p1", "cargo test") });
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "allow",
      by: "policy",
      message: null,
    });
    emit({ type: "permission_requested", request: permissionRequest("p2", "cargo build") });
    caughtUp();

    expect(find(main, 'section[aria-label="Permission request"]').textContent).toContain(
      "cargo build",
    );
    expect(find(main, '[role="log"]').textContent).toContain("Allowed by policy");
  });

  it("shows a request nobody answered in time as no longer needed, not as a denial", async () => {
    const { main, emit, caughtUp } = await openSession("/s/s1", {}, "waiting");
    emit({ type: "session_status", status: "waiting", reason: null }, null);
    emit({ type: "permission_requested", request: permissionRequest("p1", "cargo test") });
    emit({ type: "permission_requested", request: permissionRequest("p2", "rm -rf target") });
    caughtUp();
    expect(main.querySelector('section[aria-label="Permission request"]')).not.toBeNull();

    emit(
      {
        type: "permission_resolved",
        request_id: "p1",
        decision: "deny",
        by: "expired",
        message: "The turn ended before this was answered.",
      },
      null,
    );
    // `policy` is the permission mode answering, and reads as such even while the session waits.
    emit(
      {
        type: "permission_resolved",
        request_id: "p2",
        decision: "deny",
        by: "policy",
        message: null,
      },
      null,
    );
    const rows = [...find(main, '[role="log"]').querySelectorAll("li")].map(
      (row) => row.textContent,
    );
    expect(rows[0]).toContain("cargo test");
    expect(rows[0]).toContain("No longer needed");
    expect(rows[0]).toContain("The turn ended before this was answered.");
    expect(rows[0]).not.toContain("Denied");
    expect(rows[0]).not.toContain("expired");
    expect(rows[1]).toContain("Denied by policy");
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
  });

  it("says so when the server refuses the session's live updates, and asks again", async () => {
    const { main, socket } = await openSession("/s/s1");
    socket.emit("session:s1", { op: "error", topic: "session:s1", message: "replay failed" });
    flush();

    const alert = find(main, '[role="alert"]');
    expect(alert.textContent).toContain(
      "The server refused live updates for this session: replay failed",
    );
    await click(findByText(alert, "button", "Try again"));
    expect(socket.resyncs).toEqual([{ topic: "session:s1", afterSeq: 0 }]);

    socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: 0 });
    flush();
    expect(main.querySelector('[role="alert"]')).toBeNull();
  });

  it("shows the not-found screen, not a refusal, for a session that does not exist", async () => {
    const { host, socket } = await start("/s/missing", {
      getSession: () => Promise.reject(notFound()),
    });
    socket.emit("session:missing", {
      op: "error",
      topic: "session:missing",
      message: "unknown session",
    });
    flush();
    const main = find(host, "main");
    expect(main.textContent).toContain("Session not found");
    expect(main.textContent).not.toContain("refused");
  });

  describe("workspace on a phone", () => {
    beforeEach(() => setViewportWidth(PHONE));

    it("opens the panel the address names, as tabs under the header", async () => {
      const changes: RepoChanges[] = [
        {
          session_repo_id: "api",
          name: "api",
          branch: null,
          base_ref: null,
          files: [
            {
              path: "src/health.rs",
              old_path: null,
              status: "added",
              added: 9,
              removed: 0,
              binary: false,
            },
            {
              path: "src/main.rs",
              old_path: null,
              status: "modified",
              added: 2,
              removed: 1,
              binary: false,
            },
          ],
          added: 11,
          removed: 1,
        },
      ];
      const { main, emit, caughtUp, tab } = await openSession("/s/s1/changes", {
        getChanges: () => Promise.resolve(changes),
      });
      const tabs = [...main.querySelectorAll('[role="tab"]')];
      expect(tabs.map((one) => one.textContent)).toEqual([
        "Transcript",
        "Changes",
        "Files",
        "Terminal",
      ]);
      expect(tabs.map((one) => one.getAttribute("aria-selected"))).toEqual([
        "false",
        "true",
        "false",
        "false",
      ]);
      await shown(main, "2 files changed");
      const panel = find(main, `[id="${tab("Changes").getAttribute("aria-controls")}"]`);
      expect(panel.getAttribute("role")).toBe("tabpanel");
      expect(panel.getAttribute("aria-labelledby")).toBe(tab("Changes").id);
      expect(panel.textContent).toContain("src/health.rs");
      // The transcript keeps its place in the page, out of sight, and the message box with it.
      expect(find(main, '[role="log"]').closest("[hidden]")).not.toBeNull();
      expect(find(main, "textarea").closest("[hidden]")).not.toBeNull();

      emit({ type: "repo_attached", repo: makeSessionRepo("api", { primary: true }) }, null);
      emit({ type: "file_changed", session_repo_id: "api", path: "src/health.rs", op: "create" });
      emit({ type: "file_changed", session_repo_id: "api", path: "src/main.rs", op: "edit" });
      emit(
        { type: "file_changed", session_repo_id: null, path: "/tmp/notes.txt", op: "edit" },
        null,
      );
      caughtUp();
      // The server's list was read: the tab counts it, not the events. The file
      // outside every repo is no change of the session's.
      expect(tab("Changes").textContent).toBe("Changes2");
      // File changes never show up as transcript rows.
      expect(find(main, '[role="log"]').textContent).not.toContain("src/health.rs");
    });

    it("moves between panels through the address, and back with the browser", async () => {
      const { main, socket, getSession, tab } = await openSession("/s/s1");
      expect(tab("Transcript").getAttribute("aria-selected")).toBe("true");
      expect(find(main, '[role="log"]').closest("[hidden]")).toBeNull();

      await click(tab("Files"));
      expect(location.pathname).toBe("/s/s1/files");
      expect(tab("Files").getAttribute("aria-selected")).toBe("true");
      await shown(main, "No repo is attached to this session");
      const files = find(main, 'section[aria-label="File tree"]');
      expect(files.closest("[hidden]")).toBeNull();

      await click(tab("Terminal"));
      expect(location.pathname).toBe("/s/s1/terminal");
      await shown(main, "shell 1");
      // The Files panel is still there, out of sight.
      expect(files.isConnected).toBe(true);
      expect(files.closest("[hidden]")).not.toBeNull();

      history.back();
      await vi.waitFor(() => expect(location.pathname).toBe("/s/s1/files"));
      await act();
      expect(tab("Files").getAttribute("aria-selected")).toBe("true");
      expect(files.closest("[hidden]")).toBeNull();

      await click(tab("Transcript"));
      expect(location.pathname).toBe("/s/s1");
      expect(find(main, '[role="log"]').closest("[hidden]")).toBeNull();
      expect(main.querySelectorAll('[role="tabpanel"]')).toHaveLength(1);

      // The session stayed mounted through all of it: one load, one subscription.
      expect(getSession).toHaveBeenCalledTimes(1);
      expect(socket.subscriberCount("session:s1")).toBe(1);
    });

    it("keeps the draft of a message while another panel is open", async () => {
      const { main, tab } = await openSession("/s/s1");
      type(find<HTMLTextAreaElement>(main, "textarea"), "half a thought");
      await click(tab("Changes"));
      await click(tab("Transcript"));
      expect(find<HTMLTextAreaElement>(main, "textarea").value).toBe("half a thought");
    });

    it("shows a pending request under every panel", async () => {
      const { main, emit, caughtUp } = await openSession("/s/s1/terminal");
      emit({ type: "permission_requested", request: permissionRequest("p1", "cargo test") });
      caughtUp();
      const bar = find(main, 'section[aria-label="Permission request"]');
      expect(bar.closest("[hidden]")).toBeNull();
      expect(bar.textContent).toContain("cargo test");
    });

    it("has one tab stop, and arrows that move between tabs without selecting", async () => {
      const { main, tab } = await openSession("/s/s1/files");
      const tabs = [...main.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
      expect(tabs.map((one) => one.tabIndex)).toEqual([-1, -1, 0, -1]);
      const press = (key: string) => {
        document.activeElement?.dispatchEvent(
          new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
        );
      };

      tab("Files").focus();
      press("ArrowRight");
      expect(document.activeElement).toBe(tab("Terminal"));
      press("ArrowRight");
      expect(document.activeElement).toBe(tab("Transcript"));
      press("ArrowLeft");
      expect(document.activeElement).toBe(tab("Terminal"));
      press("Home");
      expect(document.activeElement).toBe(tab("Transcript"));
      press("End");
      expect(document.activeElement).toBe(tab("Terminal"));
      // Focus moved; the open panel did not.
      expect(location.pathname).toBe("/s/s1/files");

      await click(tab("Terminal"));
      expect(location.pathname).toBe("/s/s1/terminal");
      expect(tabs.map((one) => one.tabIndex)).toEqual([-1, -1, -1, 0]);
    });
  });

  describe("workspace on a wide screen", () => {
    it("keeps the transcript in view and the side pane collapsed until a panel is asked for", async () => {
      const { main, tab } = await openSession("/s/s1");
      const pane = find(main, 'aside[aria-label="Workspace panels"]');
      const tabs = [...pane.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
      // The transcript is not a tab here: it is always on the left.
      expect(tabs.map((one) => one.textContent)).toEqual(["Changes", "Files", "Terminal"]);
      expect(tabs.map((one) => one.getAttribute("aria-selected"))).toEqual([
        "false",
        "false",
        "false",
      ]);
      expect(tabs.map((one) => one.tabIndex)).toEqual([0, -1, -1]);
      expect(find(pane, '[role="tablist"]').getAttribute("aria-orientation")).toBe("vertical");
      expect(main.querySelector('[role="tabpanel"]')).toBeNull();
      expect(main.querySelector('[role="separator"]')).toBeNull();

      await click(tab("Changes"));
      expect(location.pathname).toBe("/s/s1/changes");
      await shown(pane, "No changes yet");
      expect(tab("Changes").getAttribute("aria-selected")).toBe("true");
      expect(find(pane, '[role="tablist"]').getAttribute("aria-orientation")).toBe("horizontal");
      // Both at once: the transcript and its message box stay where they were.
      expect(find(main, '[role="log"]').closest("[hidden]")).toBeNull();
      expect(find(main, "textarea").closest("[hidden]")).toBeNull();

      await click(find(pane, 'button[aria-label="Collapse the panel"]'));
      expect(location.pathname).toBe("/s/s1");
      expect(main.querySelector('[role="tabpanel"]')).toBeNull();
    });

    it("opens the side pane on the panel the address names, and resizes it from the keyboard", async () => {
      const { main, tab } = await openSession("/s/s1/terminal");
      const pane = find(main, 'aside[aria-label="Workspace panels"]');
      await shown(pane, "shell 1");
      expect(tab("Terminal").getAttribute("aria-selected")).toBe("true");

      const edge = find(main, '[role="separator"]');
      expect(edge.tabIndex).toBe(0);
      const before = Number(edge.getAttribute("aria-valuenow"));
      edge.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
      flush();
      const narrowest = Number(edge.getAttribute("aria-valuenow"));
      expect(narrowest).toBeLessThan(before);
      expect(pane.style.width).toBe(`${narrowest}px`);
      expect(narrowest).toBe(Number(edge.getAttribute("aria-valuemin")));
    });
  });

  describe("panels stay alive while the session is open", () => {
    const repos = [makeSessionRepo("api", { primary: true })];
    const disk: Partial<Api> = {
      listTree: () =>
        Promise.resolve([
          { name: "notes.md", path: "notes.md", kind: "file", size: 6, ignored: false },
        ]),
      readFile: (_id, _repo, path) =>
        Promise.resolve({
          path,
          text: "hello\n",
          binary: false,
          too_large: false,
          size: 6,
          hash: "h1",
        }),
    };
    const treeItem = (root: ParentNode, name: string) =>
      findByText(root, '[role="treeitem"]', name);

    it("keeps an unsaved edit through other panels and the transcript, on a phone", async () => {
      setViewportWidth(PHONE);
      const readFile = vi.fn(disk.readFile);
      const { main, tab } = await openSession(
        "/s/s1/files",
        { ...disk, readFile },
        "running",
        repos,
      );
      await shown(main, "notes.md");
      await click(treeItem(main, "notes.md"));
      await vi.waitFor(() => expect(editors).toHaveLength(1));
      const editor = editorFor("notes.md");
      type(editor.field, "hello, unsaved\n");

      await click(tab("Changes"));
      await shown(main, "No changes yet");
      expect(editor.field.isConnected).toBe(true);
      expect(editor.field.closest("[hidden]")).not.toBeNull();
      await click(tab("Transcript"));
      expect(find(main, '[role="log"]').closest("[hidden]")).toBeNull();
      expect(editor.field.closest("[hidden]")).not.toBeNull();
      await click(tab("Terminal"));
      await shown(main, "api 1");

      await click(tab("Files"));
      // The same editor, with the same text: nothing was mounted again or read again.
      expect(editors).toHaveLength(1);
      expect(editor.destroyed).toBe(false);
      expect(editor.field.closest("[hidden]")).toBeNull();
      expect(editor.field.value).toBe("hello, unsaved\n");
      expect(main.textContent).toContain("Unsaved changes:");
      expect(readFile).toHaveBeenCalledTimes(1);
    });

    it("keeps a shell running behind the other panels, and its output", async () => {
      setViewportWidth(PHONE);
      const { main, tab, shells } = await openSession("/s/s1/terminal", disk, "running", repos);
      await vi.waitFor(() => expect(shells).toHaveLength(1));
      const [shell] = shells;
      const [view] = views;
      shell.handlers.onOpen?.();
      shell.handlers.onData(new TextEncoder().encode("api $ "));

      await click(tab("Files"));
      await shown(main, "notes.md");
      await click(tab("Transcript"));
      await click(tab("Changes"));
      expect(shell.closed).toBe(false);
      expect(view.disposed).toBe(false);
      expect(view.element.closest("[hidden]")).not.toBeNull();
      // Output that arrives meanwhile is on the screen when the reader comes back.
      shell.handlers.onData(new TextEncoder().encode("make: done\r\n"));

      await click(tab("Terminal"));
      expect(view.element.closest("[hidden]")).toBeNull();
      expect(view.screen).toBe("api $ make: done\r\n");
      expect(shells).toHaveLength(1);
      expect(views).toHaveLength(1);
      view.options.onInput("ls\r");
      expect(shell.input).toBe("ls\r");
    });

    it("keeps both through the side pane collapsing and opening again, on a wide screen", async () => {
      const { main, tab, shells } = await openSession("/s/s1/files", disk, "running", repos);
      const pane = find(main, 'aside[aria-label="Workspace panels"]');
      await shown(pane, "notes.md");
      await click(treeItem(pane, "notes.md"));
      await vi.waitFor(() => expect(editors).toHaveLength(1));
      const editor = editorFor("notes.md");
      type(editor.field, "wide and unsaved\n");
      await click(tab("Terminal"));
      await vi.waitFor(() => expect(shells).toHaveLength(1));

      await click(find(pane, 'button[aria-label="Collapse the panel"]'));
      expect(location.pathname).toBe("/s/s1");
      expect(main.querySelector('[role="tabpanel"]')).toBeNull();
      expect(editor.field.isConnected).toBe(true);
      expect(shells[0].closed).toBe(false);

      await click(tab("Files"));
      expect(editor.field.closest("[hidden]")).toBeNull();
      expect(editor.field.value).toBe("wide and unsaved\n");
      expect(editors).toHaveLength(1);
      expect(shells).toHaveLength(1);
      expect(shells[0].closed).toBe(false);
    });

    it("loads nothing for a panel that was never opened", async () => {
      const listTree = vi.fn(disk.listTree);
      const { main, shells } = await openSession(
        "/s/s1/changes",
        { ...disk, listTree },
        "running",
        repos,
      );
      await shown(main, "No changes yet");
      await act();
      expect(shells).toHaveLength(0);
      expect(views).toHaveLength(0);
      expect(listTree).not.toHaveBeenCalled();
      expect(main.querySelector('[role="tree"]')).toBeNull();
    });

    it("opens no terminal for a session that does not exist", async () => {
      const { main, shells } = await openSession("/s/s1/terminal", {
        getSession: () => Promise.reject(notFound()),
      });
      await act();
      expect(main.textContent).toContain("Session not found");
      expect(shells).toHaveLength(0);
    });

    it("closes the shells and drops the editors when the session screen goes away", async () => {
      const { host, main, tab, shells } = await openSession(
        "/s/s1/terminal",
        disk,
        "running",
        repos,
      );
      await vi.waitFor(() => expect(shells).toHaveLength(1));
      await click(tab("Files"));
      await shown(main, "notes.md");
      await click(findByText<HTMLAnchorElement>(host, "a", "Calin"));
      await act();
      expect(location.pathname).toBe("/");
      expect(shells[0].closed).toBe(true);
      expect(views[0].disposed).toBe(true);
    });

    it("opens a changed file in the Files panel from its diff", async () => {
      const changes: RepoChanges[] = [
        {
          session_repo_id: "api",
          name: "api",
          branch: null,
          base_ref: null,
          files: [
            {
              path: "notes.md",
              old_path: null,
              status: "modified",
              added: 1,
              removed: 1,
              binary: false,
            },
          ],
          added: 1,
          removed: 1,
        },
      ];
      const { main, tab } = await openSession(
        "/s/s1/changes",
        {
          ...disk,
          getChanges: () => Promise.resolve(changes),
          getDiff: (_id, _repo, path) =>
            Promise.resolve({
              path,
              old_path: null,
              old: "hi\n",
              new: "hello\n",
              binary: false,
              too_large: false,
            }),
        },
        "running",
        repos,
      );
      await shown(main, "1 file changed");
      await click(findByText(main, "li > button", "notes.md"));
      await act();
      await click(findByText(main, "button", "Open in Files"));
      await vi.waitFor(() => expect(location.pathname).toBe("/s/s1/files"));
      expect(tab("Files").getAttribute("aria-selected")).toBe("true");
      await vi.waitFor(() => expect(editorFor("notes.md").field.value).toBe("hello\n"));
      expect(editorFor("notes.md").field.closest("[hidden]")).toBeNull();
    });

    it("asks before a link leaves the session while a file has unsaved changes", async () => {
      const { host, main } = await openSession("/s/s1/files", disk, "running", repos);
      await shown(main, "notes.md");
      await click(treeItem(main, "notes.md"));
      await vi.waitFor(() => expect(editors).toHaveLength(1));
      type(editorFor("notes.md").field, "unsaved\n");
      // happy-dom has no dialogs of its own.
      const confirm = vi.fn<(message?: string) => boolean>().mockReturnValue(false);
      vi.stubGlobal("confirm", confirm);

      await click(findByText<HTMLAnchorElement>(host, "a", "Calin"));
      await act();
      expect(confirm).toHaveBeenCalledWith(
        "A file has unsaved changes. Leave this session and lose them?",
      );
      expect(location.pathname).toBe("/s/s1/files");
      expect(editorFor("notes.md").field.value).toBe("unsaved\n");

      // Moving between the session's own panels loses nothing and asks nothing.
      confirm.mockClear();
      await click(findByText<HTMLButtonElement>(main, '[role="tab"]', "Changes"));
      expect(location.pathname).toBe("/s/s1/changes");
      expect(confirm).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      await click(findByText<HTMLAnchorElement>(host, "a", "Calin"));
      await act();
      expect(location.pathname).toBe("/");
    });
  });

  it("keeps the open session marked in the list while one of its panels is open", async () => {
    const { host, listSessions } = await openSession("/s/s1/changes");
    listSessions(makeSession("s1"), makeSession("s2", 2000));
    const marked = [...host.querySelectorAll<HTMLAnchorElement>("nav a")].map((link) => [
      link.getAttribute("href"),
      link.hasAttribute("data-active"),
    ]);
    expect(marked).toEqual([
      ["/s/s2", false],
      ["/s/s1", true],
    ]);
  });

  it("answers an address with an unknown panel with the not-found page", async () => {
    const { host } = await start("/s/s1/settings", {
      getSession: () => Promise.resolve(makeDetail(makeSession("s1"))),
    });
    expect(find(host, "main").textContent).toContain("Page not found");
  });
});

describe("App: settings and the catalog", () => {
  const stored: Settings = { home_repo: "/code/notes", scan_roots: ["/code"], clone_root: null };
  const home = makeRepo("notes", { home: true });
  const api = makeRepo("api", { declared: true });
  const web = makeMissingRepo("web");

  it("reaches the settings from the header, and comes back", async () => {
    setViewportWidth(PHONE);
    const getSettings = vi.fn<Api["getSettings"]>().mockResolvedValue(stored);
    const { host } = await start("/", { getSettings, listRepos: () => Promise.resolve([home]) });
    const link = findByText<HTMLAnchorElement>(find(host, "aside"), "a", "Settings");
    expect(link.getAttribute("href")).toBe("/settings");
    expect(getSettings).not.toHaveBeenCalled();

    await click(link);
    await vi.waitFor(() => expect(location.pathname).toBe("/settings"));
    await act();
    const main = find(host, "main");
    expect(find(main, "h1").textContent).toBe("Settings");
    expect(control(main, "Home repo").value).toBe("/code/notes");
    // On a phone the page has the screen: the sessions list steps aside.
    expect(find(host, "aside").classList.contains("hidden")).toBe(true);
    expect(main.querySelector("textarea")).toBeNull();

    await click(find<HTMLAnchorElement>(main, 'a[aria-label="Back to sessions"]'));
    await vi.waitFor(() => expect(location.pathname).toBe("/"));
    await act();
    expect(find(host, "main").querySelector("textarea")).not.toBeNull();
  });

  it("opens the settings at their own address, beside the sessions on a wide screen", async () => {
    const { host } = await start("/settings", { getSettings: () => Promise.resolve(stored) });
    expect(find(host, "main").textContent).toContain("Scan roots");
    expect(find(host, "aside").textContent).toContain("Calin");
    expect(
      findByText<HTMLAnchorElement>(find(host, "aside"), "a", "Settings").getAttribute(
        "aria-current",
      ),
    ).toBe("page");
  });

  it("starts a session in the home repo by default, with a set's repos added on request", async () => {
    const created = makeSession("new", 5000, { status: "queued" });
    const createSession = vi.fn<Api["createSession"]>().mockResolvedValue(makeDetail(created));
    const { host } = await start("/", {
      // In whatever order the server answers, the home repo leads.
      listRepos: () => Promise.resolve([web, api, home]),
      listRepoSets: () => Promise.resolve([{ name: "storefront", repo_ids: ["api", "web"] }]),
      createSession,
      getSession: () => Promise.resolve(makeDetail(created)),
    });
    const main = find(host, "main");
    const names = [...main.querySelectorAll("section ul > li > div:first-child > label")].map(
      (label) => label.textContent,
    );
    expect(names).toEqual(["notesHomePrimary/code/notes", "api/code/api", "webnot cloned"]);

    await click(findByText(main, "button", "storefront"));
    expect(main.textContent).toContain("storefront: web is not cloned and was left out.");
    type(find<HTMLTextAreaElement>(main, "textarea"), "Rename the endpoint");
    await click(findByText(main, "button", "Start"));
    expect(createSession).toHaveBeenCalledWith({
      prompt: "Rename the endpoint",
      permission_mode: "ask",
      repos: [
        { repo_id: "notes", mode: "live" },
        { repo_id: "api", mode: "write" },
      ],
      runner: "mock",
    });
  });

  describe("cloning a repo", () => {
    beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
    afterEach(() => vi.useRealTimers());

    const row = (main: HTMLElement) => {
      const found = findByText(main, "section ul > li", "web");
      return {
        box: find<HTMLInputElement>(found, 'input[type="checkbox"]'),
        text: () => found.textContent,
        cloneButton: () => found.querySelector<HTMLButtonElement>('button[aria-label="Clone web"]'),
      };
    };
    const tick = async (ms: number): Promise<void> => {
      await vi.advanceTimersByTimeAsync(ms);
      flush();
    };

    it("asks for it, reads the list every two seconds while it runs, and stops when it is done", async () => {
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([home, api, web]);
      const cloneRepo = vi.fn<Api["cloneRepo"]>().mockResolvedValue();
      const { host } = await start("/", { listRepos, cloneRepo });
      const main = find(host, "main");
      const repo = row(main);
      expect(repo.box.disabled).toBe(true);
      expect(listRepos).toHaveBeenCalledTimes(1);
      // Nothing is cloning: nothing is polled.
      await tick(10_000);
      expect(listRepos).toHaveBeenCalledTimes(1);

      listRepos.mockResolvedValue([home, api, { ...web, cloning: true }]);
      const button = repo.cloneButton();
      if (!button) throw new Error("web cannot be cloned");
      await click(button);
      expect(cloneRepo).toHaveBeenCalledWith("web");
      expect(repo.text()).toContain("Cloning…");
      expect(repo.cloneButton()).toBeNull();

      await tick(1999);
      expect(listRepos).toHaveBeenCalledTimes(1);
      await tick(1);
      expect(listRepos).toHaveBeenCalledTimes(2);
      await tick(2000);
      expect(listRepos).toHaveBeenCalledTimes(3);
      expect(repo.text()).toContain("Cloning…");
      expect(repo.box.disabled).toBe(true);

      listRepos.mockResolvedValue([home, api, makeRepo("web", { declared: true, url: web.url })]);
      await tick(2000);
      expect(listRepos).toHaveBeenCalledTimes(4);
      const cloned = row(main);
      expect(cloned.text()).not.toContain("Cloning…");
      expect(cloned.text()).toContain("/code/web");
      expect(cloned.box.disabled).toBe(false);

      await tick(20_000);
      expect(listRepos).toHaveBeenCalledTimes(4);
    });

    it("shows why a clone failed and stops asking, until it is tried again", async () => {
      const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([home, web]);
      const cloneRepo = vi.fn<Api["cloneRepo"]>().mockResolvedValue();
      const { host } = await start("/", { listRepos, cloneRepo });
      const main = find(host, "main");

      listRepos.mockResolvedValue([home, { ...web, cloning: true }]);
      const button = row(main).cloneButton();
      if (!button) throw new Error("web cannot be cloned");
      await click(button);
      await tick(2000);
      const failed: Repo = { ...web, problem: "git clone failed: could not read from remote" };
      listRepos.mockResolvedValue([home, failed]);
      await tick(2000);

      const repo = row(main);
      expect(repo.text()).toContain("git clone failed: could not read from remote");
      expect(repo.text()).not.toContain("Cloning…");
      expect(repo.box.disabled).toBe(true);
      const reads = listRepos.mock.calls.length;
      await tick(20_000);
      expect(listRepos).toHaveBeenCalledTimes(reads);

      // The row offers the clone again.
      const again = repo.cloneButton();
      if (!again) throw new Error("the clone is not offered again");
      await click(again);
      expect(cloneRepo).toHaveBeenCalledTimes(2);
      expect(row(main).text()).toContain("Cloning…");
    });
  });
});

describe("App: attaching a repo to a session under way", () => {
  const attached = [makeSessionRepo("api", { primary: true, repo_id: "api" })];
  const catalog = [
    makeRepo("notes", { home: true }),
    makeRepo("api"),
    makeRepo("docs"),
    makeMissingRepo("mobile"),
    makeRepo("web"),
  ];
  const attachButton = (main: HTMLElement) =>
    findByText<HTMLButtonElement>(find(main, "header"), "button", "Attach repo");
  const picker = (main: HTMLElement) =>
    find<HTMLFormElement>(main, 'form[aria-label="Attach a repo"]');
  const chips = (main: HTMLElement) =>
    [...find(main, "header").querySelectorAll('ul[aria-label="Repos"] > li')].map(
      (chip) => chip.textContent,
    );

  it("offers it while the session is running, waiting or in review, and at no other time", async () => {
    const { main, emit, caughtUp } = await openSession("/s/s1", {}, "queued", attached);
    caughtUp();
    const button = attachButton(main);
    const expected = {
      queued: false,
      preparing: false,
      running: true,
      waiting: true,
      review: true,
      shipping: false,
      done: false,
      failed: false,
      cancelled: false,
    } as const;
    for (const [status, can] of Object.entries(expected)) {
      emit({ type: "session_status", status: status as SessionStatus, reason: null }, null);
      expect([status, !button.disabled]).toEqual([status, can]);
    }
    expect(button.title).toBe(
      "A repo can be attached while the session is running, waiting or in review",
    );
  });

  it("picks among the cloned repos not yet attached, sends the request, and waits for the event", async () => {
    const attachRepo = vi
      .fn<Api["attachRepo"]>()
      .mockResolvedValue(makeSessionRepo("web", { repo_id: "web", mode: "read" }));
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue(catalog);
    const { main, emit, caughtUp } = await openSession(
      "/s/s1",
      { attachRepo, listRepos },
      "running",
      attached,
    );
    emit({ type: "session_status", status: "running", reason: null }, null);
    caughtUp();
    expect(main.querySelector('form[aria-label="Attach a repo"]')).toBeNull();
    expect(listRepos).not.toHaveBeenCalled();

    await click(attachButton(main));
    expect(attachButton(main).getAttribute("aria-expanded")).toBe("true");
    const form = picker(main);
    const repo = control<HTMLSelectElement>(form, "Repo to attach");
    // Not the one already attached, nor the one that is not cloned.
    expect([...repo.options].map((option) => option.textContent)).toEqual([
      "notes (home)",
      "docs",
      "web",
    ]);
    const mode = control<HTMLSelectElement>(form, "Attached as");
    expect([...mode.options].map((option) => option.textContent)).toEqual([
      "Worktree",
      "Read only",
      "Edit in place",
    ]);
    // The home repo is edited in place unless told otherwise; any other repo gets a worktree.
    expect(mode.value).toBe("live");
    choose(repo, "web");
    expect(mode.value).toBe("write");
    choose(mode, "read");

    await click(findByText(form, "button", "Attach"));
    expect(attachRepo).toHaveBeenCalledWith("s1", { repo_id: "web", mode: "read" });
    expect(main.querySelector('form[aria-label="Attach a repo"]')).toBeNull();
    // The server said yes, but the repo set is the log's: nothing is added before the event.
    expect(chips(main)).toEqual(["apiprimaryworktree"]);
    expect(find(main, '[role="log"]').textContent).not.toContain("attached");

    emit(
      { type: "repo_attached", repo: makeSessionRepo("web", { repo_id: "web", mode: "read" }) },
      null,
    );
    expect(chips(main)).toEqual(["apiprimaryworktree", "webread only"]);
    expect(find(main, '[role="log"]').textContent).toContain(
      "Repo web attached by you · Read only",
    );

    // It is no longer offered.
    await click(attachButton(main));
    expect(
      [...control<HTMLSelectElement>(picker(main), "Repo to attach").options].map(
        (option) => option.value,
      ),
    ).toEqual(["notes", "docs"]);
    await click(findByText(picker(main), "button", "Cancel"));
    expect(main.querySelector('form[aria-label="Attach a repo"]')).toBeNull();
  });

  it("shows the server's message when the attach is refused, and keeps the picker open", async () => {
    const attachRepo = vi
      .fn<Api["attachRepo"]>()
      .mockRejectedValue(conflict("docs is attached to another session in place"));
    const { main, emit, caughtUp } = await openSession(
      "/s/s1",
      { attachRepo, listRepos: () => Promise.resolve(catalog) },
      "running",
      attached,
    );
    emit({ type: "session_status", status: "review", reason: null }, null);
    caughtUp();
    await click(attachButton(main));
    choose(control<HTMLSelectElement>(picker(main), "Repo to attach"), "docs");
    await click(findByText(picker(main), "button", "Attach"));

    expect(attachRepo).toHaveBeenCalledWith("s1", { repo_id: "docs", mode: "write" });
    expect(find(picker(main), '[role="alert"]').textContent).toBe(
      "docs is attached to another session in place",
    );
    expect(findByText<HTMLButtonElement>(picker(main), "button", "Attach").disabled).toBe(false);
    expect(chips(main)).toEqual(["apiprimaryworktree"]);
  });

  it("says so when there is nothing left to attach, or the list cannot be read", async () => {
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue([makeRepo("api"), makeMissingRepo("mobile")]);
    const { main, emit, caughtUp } = await openSession("/s/s1", { listRepos }, "running", attached);
    emit({ type: "session_status", status: "running", reason: null }, null);
    caughtUp();
    await click(attachButton(main));
    expect(find(picker(main), '[role="alert"]').textContent).toContain(
      "The repos could not be loaded: Cannot reach the server",
    );
    expect(findByText<HTMLButtonElement>(picker(main), "button", "Attach").disabled).toBe(true);

    await click(findByText(picker(main), "button", "Retry"));
    await act();
    expect(picker(main).textContent).toContain(
      "Every repo that is cloned on this machine is already attached to this session.",
    );
    expect(findByText<HTMLButtonElement>(picker(main), "button", "Attach").disabled).toBe(true);
  });

  it("closes the picker to further attaching once the session can take none", async () => {
    const attachRepo = vi.fn<Api["attachRepo"]>().mockResolvedValue(makeSessionRepo("web"));
    const { main, emit, caughtUp } = await openSession(
      "/s/s1",
      { attachRepo, listRepos: () => Promise.resolve(catalog) },
      "running",
      attached,
    );
    emit({ type: "session_status", status: "running", reason: null }, null);
    caughtUp();
    await click(attachButton(main));
    emit({ type: "session_status", status: "done", reason: null }, null);

    const submit = findByText<HTMLButtonElement>(picker(main), "button", "Attach");
    expect(submit.disabled).toBe(true);
    expect(picker(main).textContent).toContain(
      "A repo can be attached while the session is running, waiting or in review.",
    );
    await click(submit);
    expect(attachRepo).not.toHaveBeenCalled();
  });

  it("says in the transcript who attached a repo, for those attached after the first turn began", async () => {
    const { main, emit, caughtUp } = await openSession("/s/s1");
    emit({ type: "session_status", status: "preparing", reason: null }, null);
    emit({ type: "repo_attached", repo: makeSessionRepo("api", { primary: true }) }, null);
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({ type: "repo_attached", repo: makeSessionRepo("web") }, null);
    emit({ type: "repo_attached", repo: makeSessionRepo("docs", { mode: "read" }) }, "a1");
    caughtUp();

    const rows = [...find(main, '[role="log"]').querySelectorAll("ol > li")].map(
      (row) => row.textContent,
    );
    // The repo the session started with is not news.
    expect(rows).toEqual([
      "Repo web attached by you · Worktree",
      "Repo docs attached by the agent · Read only",
    ]);
  });

  describe("the panels", () => {
    const web = makeSessionRepo("web", { repo_id: "web" });
    const listing = (...repos: string[]): RepoChanges[] =>
      repos.map((name) => ({
        session_repo_id: name,
        name,
        branch: null,
        base_ref: null,
        files: [
          {
            path: `${name}.md`,
            old_path: null,
            status: "modified",
            added: 1,
            removed: 0,
            binary: false,
          },
        ],
        added: 1,
        removed: 0,
      }));

    it("Changes lists the new repo without a reload", async () => {
      const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue(listing("api"));
      const { main, emit, caughtUp } = await openSession(
        "/s/s1/changes",
        { getChanges },
        "running",
        attached,
      );
      emit({ type: "session_status", status: "running", reason: null }, null);
      caughtUp();
      await shown(main, "api.md");
      const panel = find(main, 'section[aria-label="Changed files"]');
      expect(panel.querySelector('section[aria-label="web"]')).toBeNull();

      getChanges.mockResolvedValue(listing("api", "web"));
      emit({ type: "repo_attached", repo: web }, null);
      await shown(panel, "web.md");
      expect(find(panel, 'section[aria-label="web"]').textContent).toContain("web.md");
      expect(panel.textContent).toContain("2 files changed in 2 of 2 repos");
    });

    it("Files shows the new repo as a root of the tree, and reads it when opened", async () => {
      const listTree = vi.fn<Api["listTree"]>((_id, repo) =>
        Promise.resolve([
          { name: `${repo}.md`, path: `${repo}.md`, kind: "file", size: 1, ignored: false },
        ]),
      );
      const { main, emit, caughtUp } = await openSession(
        "/s/s1/files",
        { listTree },
        "running",
        attached,
      );
      emit({ type: "session_status", status: "running", reason: null }, null);
      caughtUp();
      await shown(main, "api.md");
      const roots = () =>
        [...main.querySelectorAll('[role="treeitem"][aria-level="1"]')].map((root) =>
          root.textContent.trim(),
        );
      expect(roots()).toHaveLength(1);

      emit({ type: "repo_attached", repo: web }, null);
      expect(roots()).toHaveLength(2);
      expect(roots()[1]).toContain("web");
      await click(findByText(main, '[role="treeitem"][aria-level="1"]', "web"));
      await shown(main, "web.md");
      expect(listTree).toHaveBeenCalledWith("s1", "web", "");
    });

    it("Terminal offers the new repo as a place to start a shell", async () => {
      const { main, emit, caughtUp, shells } = await openSession(
        "/s/s1/terminal",
        {},
        "running",
        attached,
      );
      emit({ type: "session_status", status: "running", reason: null }, null);
      caughtUp();
      // One repo: the first shell starts in it without asking.
      await vi.waitFor(() => expect(shells).toHaveLength(1));
      expect(main.querySelector('form[aria-label="New terminal"]')).toBeNull();

      emit({ type: "repo_attached", repo: web }, null);
      await click(findByText(main, "button", "New terminal"));
      const form = find<HTMLFormElement>(main, 'form[aria-label="New terminal"]');
      const where = find<HTMLSelectElement>(form, "select");
      expect([...where.options].map((option) => option.value)).toEqual(["api", "web"]);
      choose(where, "web");
      await click(findByText(form, "button", "Start"));
      await vi.waitFor(() => expect(shells).toHaveLength(2));
      expect(shells[1].request.sessionRepoId).toBe("web");
    });
  });
});

describe("App: the Changes tab's count", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
  afterEach(() => vi.useRealTimers());

  const listing = (...paths: string[]): RepoChanges[] => [
    {
      session_repo_id: "api",
      name: "api",
      branch: null,
      base_ref: null,
      files: paths.map((path) => ({
        path,
        old_path: null,
        status: "modified",
        added: 1,
        removed: 0,
        binary: false,
      })),
      added: paths.length,
      removed: 0,
    },
  ];
  const tick = async (ms: number): Promise<void> => {
    await vi.advanceTimersByTimeAsync(ms);
    flush();
  };

  it("counts the events until the server's list is read, then the list, with no panel ever opened", async () => {
    // The agent edited two files through its shell: no event names them.
    const getChanges = vi
      .fn<Api["getChanges"]>()
      .mockResolvedValue(listing("a.rs", "shell-1.rs", "shell-2.rs"));
    const { main, emit, caughtUp, tab } = await openSession("/s/s1", { getChanges }, "running", [
      makeSessionRepo("api", { primary: true }),
    ]);
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    caughtUp();
    // Not read yet: the count is what the events named.
    expect(getChanges).not.toHaveBeenCalled();
    expect(tab("Changes").textContent).toBe("Changes1");

    await tick(500);
    expect(getChanges).toHaveBeenCalledTimes(1);
    expect(tab("Changes").textContent).toBe("Changes3");
    // The panel itself was never loaded.
    expect(main.querySelector('section[aria-label="Changed files"]')).toBeNull();

    // An edit the list does not hold yet is added to it, once.
    emit({ type: "file_changed", session_repo_id: "api", path: "b.rs", op: "create" });
    emit({ type: "file_changed", session_repo_id: "api", path: "b.rs", op: "edit" });
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    expect(tab("Changes").textContent).toBe("Changes4");
    await tick(5000);
    expect(getChanges).toHaveBeenCalledTimes(1);

    // The turn ends: the list is read again, panel or no panel. One file was reverted meanwhile.
    getChanges.mockResolvedValue(listing("a.rs", "b.rs"));
    emit({ type: "session_status", status: "review", reason: null }, null);
    expect(getChanges).toHaveBeenCalledTimes(1);
    await tick(500);
    expect(getChanges).toHaveBeenCalledTimes(2);
    expect(tab("Changes").textContent).toBe("Changes2");
  });

  it("shows no count when the server lists nothing, whatever the events said", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockResolvedValue(listing());
    const { emit, caughtUp, tab } = await openSession("/s/s1", { getChanges });
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    caughtUp();
    expect(tab("Changes").textContent).toBe("Changes1");
    await tick(500);
    // Written, then undone: git has nothing to show.
    expect(tab("Changes").textContent).toBe("Changes");
  });

  it("keeps the events' count when the list cannot be read", async () => {
    const getChanges = vi.fn<Api["getChanges"]>().mockRejectedValue(unreachable());
    const { emit, caughtUp, tab } = await openSession("/s/s1", { getChanges });
    emit({ type: "file_changed", session_repo_id: "api", path: "a.rs", op: "edit" });
    caughtUp();
    await tick(500);
    expect(getChanges).toHaveBeenCalledTimes(1);
    expect(tab("Changes").textContent).toBe("Changes1");
  });
});

describe("App: a tool call, its permission and its files", () => {
  const repos = [makeSessionRepo("api", { primary: true })];
  const call = (status: Item["status"], locations: string[] = []): Item => ({
    id: "t1",
    status,
    content: {
      kind: "tool",
      name: "Edit",
      tool_kind: "edit",
      title: "notes.md",
      input: { file_path: "/sessions/s1/api/notes.md" },
      output: null,
      locations,
    },
  });
  const request: PermissionRequest = {
    id: "p1",
    tool_name: "Edit",
    tool_kind: "edit",
    title: "notes.md",
    input: { file_path: "/sessions/s1/api/notes.md" },
    item_id: "t1",
  };

  it("asks in the call's card and in the bar, and an answer in one holds the other", async () => {
    const resolvePermission = vi.fn<Api["resolvePermission"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await openSession("/s/s1", { resolvePermission });
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({ type: "item_started", item: call("running") });
    emit({ type: "permission_requested", request });
    emit({ type: "session_status", status: "waiting", reason: null }, null);
    caughtUp();

    const log = find(main, '[role="log"]');
    // One card for the call and its request, not two.
    expect(log.querySelectorAll("ol > li")).toHaveLength(1);
    const question = find(log, '[data-permission="pending"]');
    expect(question.textContent).toContain("Allow this?");
    const bar = find(main, 'section[aria-label="Permission request"]');
    expect(bar.textContent).toContain("Allow this?");
    // The bar's two buttons are the only ones named plainly Allow and Deny.
    const named = (name: string) =>
      [...main.querySelectorAll("button")].filter(
        (button) => (button.getAttribute("aria-label") ?? button.textContent.trim()) === name,
      );
    expect(named("Allow")).toEqual([findByText(bar, "button", "Allow")]);
    expect(named("Deny")).toEqual([findByText(bar, "button", "Deny")]);

    await click(find(question, 'button[aria-label="Allow notes.md"]'));
    expect(resolvePermission).toHaveBeenCalledTimes(1);
    expect(resolvePermission).toHaveBeenCalledWith("s1", "p1", { decision: "allow" });
    // Sent from the card: the bar knows, and will not send a second answer.
    expect(findByText<HTMLButtonElement>(bar, "button", "Allow").disabled).toBe(true);
    expect(findByText<HTMLButtonElement>(bar, "button", "Deny").disabled).toBe(true);
    expect(bar.textContent).toContain("Sent, waiting for the agent…");

    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "allow",
      by: "user",
      message: null,
    });
    emit({ type: "session_status", status: "running", reason: null }, null);
    emit({ type: "item_completed", item: call("completed") });
    expect(main.querySelector('section[aria-label="Permission request"]')).toBeNull();
    expect(log.querySelectorAll("ol > li")).toHaveLength(1);
    expect(find(log, "[data-permission]").textContent).toBe("Allowed by you");
    expect(log.textContent).not.toContain("Allow this?");
  });

  it("records a denial given in the bar, with its reason, in the call's card", async () => {
    const resolvePermission = vi.fn<Api["resolvePermission"]>().mockResolvedValue();
    const { main, emit, caughtUp } = await openSession("/s/s1", { resolvePermission });
    emit({ type: "item_started", item: call("running") });
    emit({ type: "permission_requested", request });
    emit({ type: "session_status", status: "waiting", reason: null }, null);
    caughtUp();
    const bar = find(main, 'section[aria-label="Permission request"]');
    await click(findByText(bar, "button", "Deny"));
    type(control(bar, "Reason for the agent"), "Not that file.");
    await click(findByText(bar, "button", "Confirm deny"));
    // The card's own buttons hold while the answer is on its way to the log.
    const log = find(main, '[role="log"]');
    expect(find<HTMLButtonElement>(log, 'button[aria-label="Allow notes.md"]').disabled).toBe(true);

    emit({
      type: "permission_resolved",
      request_id: "p1",
      decision: "deny",
      by: "user",
      message: "Not that file.",
    });
    emit({ type: "item_completed", item: call("declined") });
    expect(find(log, "[data-permission]").textContent).toBe("Denied by you: Not that file.");
    expect(find(log, "summary").textContent).toContain("Declined");
  });

  it("opens a file the call names in the Files panel", async () => {
    setViewportWidth(PHONE);
    const { main, emit, caughtUp, tab } = await openSession(
      "/s/s1",
      {
        listTree: () => Promise.resolve([]),
        readFile: (_id, _repo, path) =>
          Promise.resolve({
            path,
            text: "hello\n",
            binary: false,
            too_large: false,
            size: 6,
            hash: "h1",
          }),
      },
      "running",
      repos,
    );
    emit({
      type: "item_completed",
      item: call("completed", ["/sessions/s1/api/docs/notes.md", "/var/log/syslog"]),
    });
    caughtUp();
    const card = find<HTMLDetailsElement>(find(main, '[role="log"]'), "details");
    card.open = true;
    card.dispatchEvent(new Event("toggle"));
    flush();
    const files = find(card, 'ul[aria-label="Files of this tool call"]');
    expect([...files.children].map((row) => row.textContent)).toEqual([
      "api/docs/notes.md",
      "/var/log/syslog",
    ]);

    await click(find(files, "button"));
    await vi.waitFor(() => expect(location.pathname).toBe("/s/s1/files"));
    expect(tab("Files").getAttribute("aria-selected")).toBe("true");
    await vi.waitFor(() => expect(editorFor("docs/notes.md").field.value).toBe("hello\n"));
  });
});
