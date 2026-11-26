import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Health } from "./gen/Health";
import { createAppState } from "./state/app";
import {
  fakeApi,
  FakeSocket,
  immediately,
  makeDetail,
  makeEvent,
  makeSession,
  notFound,
  settle,
  unreachable,
} from "./test/fakes";
import { find, findByText, mount, type, unmountAll } from "./test/render";

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
  const api = fakeApi({ health: () => Promise.resolve(health), ...routes });
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
    expect(box.disabled).toBe(true);
    expect(send.disabled).toBe(true);
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
    const stop = findByText<HTMLButtonElement>(main, "button", "Stop");
    const box = find<HTMLTextAreaElement>(main, "textarea");
    socket.emit("session:s1", { op: "subscribed", topic: "session:s1", head_seq: 0 });

    const expected = {
      queued: { stop: true, message: false },
      preparing: { stop: true, message: false },
      running: { stop: true, message: true },
      waiting: { stop: true, message: true },
      review: { stop: false, message: true },
      shipping: { stop: false, message: false },
      done: { stop: false, message: false },
      failed: { stop: false, message: false },
      cancelled: { stop: false, message: false },
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
      expect({ status, stop: !stop.disabled, message: !box.disabled }).toEqual({ status, ...can });
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
});
