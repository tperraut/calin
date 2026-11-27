import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Health } from "./gen/Health";
import { createAppState } from "./state/app";
import type { PermissionRequest } from "./gen/PermissionRequest";
import type { SessionStatus } from "./gen/SessionStatus";
import {
  conflict,
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeRepo,
  makeSession,
  makeSessionRepo,
  notFound,
  settle,
  unreachable,
} from "./test/fakes";
import {
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

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock", "claude"],
};

/** Mounts the whole app at `path` over fakes, with the socket already open. */
async function start(path: string, routes: Partial<Api> = {}) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    ...routes,
  });
  const state = createAppState({ api, socket, schedule: immediately });
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
) {
  const session = makeSession("s1", 1000, { title: "Add a health check", status });
  const getSession = vi.fn<Api["getSession"]>().mockResolvedValue(makeDetail(session));
  const app = await start(path, { getSession, ...routes });
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
  return { ...app, main, emit, caughtUp, getSession, tab };
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
});

afterEach(unmountAll);

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
      const { main, emit, caughtUp, tab } = await openSession("/s/s1/changes");
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
      await shown(main, "No file has been changed yet.");
      const panel = find(main, `[id="${tab("Changes").getAttribute("aria-controls")}"]`);
      expect(panel.getAttribute("role")).toBe("tabpanel");
      expect(panel.getAttribute("aria-labelledby")).toBe(tab("Changes").id);
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
      expect(tab("Changes").textContent).toBe("Changes3");
      const groups = [...panel.querySelectorAll("section")].map((group) => group.textContent);
      expect(groups).toHaveLength(2);
      expect(groups[0]).toContain("api");
      expect(groups[0]).toContain("2 files");
      expect(groups[0]).toContain("src/health.rs");
      expect(groups[0]).toContain("Created by the agent");
      expect(groups[1]).toContain("Outside the session's repos");
      expect(groups[1]).toContain("Edited by you");
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
      await shown(main, "Browse the session's repos");

      await click(tab("Terminal"));
      expect(location.pathname).toBe("/s/s1/terminal");
      await shown(main, "A shell in the session's folder");
      expect(main.textContent).not.toContain("Browse the session's repos");

      history.back();
      await vi.waitFor(() => expect(location.pathname).toBe("/s/s1/files"));
      await act();
      expect(tab("Files").getAttribute("aria-selected")).toBe("true");
      await shown(main, "Browse the session's repos");

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
      await shown(pane, "No file has been changed yet.");
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
      await shown(pane, "A shell in the session's folder");
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
