import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import { createApi, type FetchLike } from "./api/http";
import { createSocket, type WebSocketLike } from "./api/socket";
import type { Health } from "./gen/Health";
import type { Me } from "./gen/Me";
import type { Settings } from "./gen/Settings";
import { createAppState } from "./state/app";
import { deferred, FakeSocket, immediately, settle } from "./test/fakes";
import { click, find, findByText, mount, unmountAll } from "./test/render";

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: true,
  runners: ["mock"],
  github_problem: null,
};
const settings: Settings = {
  home_repo: null,
  scan_roots: [],
  clone_root: null,
  max_parallel: null,
};
const localMe: Me = { mode: "local", identity: null, device_token: false };
const publicMe: Me = { mode: "public", identity: "ada@example.org", device_token: false };

const json = (body: unknown, status = 200): Response =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const signedOut = (): Response => json({ error: "sign-in required" }, 401);

type Answer = Response | Promise<Response> | (() => Response | Promise<Response>);

/**
 * A server over `fetch`: `answers` maps "METHOD path" to a response; anything
 * else fails the test. Every call is recorded in order.
 */
function server(answers: Record<string, Answer>) {
  const calls: string[] = [];
  const fetchFn = vi.fn<FetchLike>((input, init) => {
    const key = `${init?.method ?? "GET"} ${input}`;
    calls.push(key);
    const answer = answers[key];
    if (answer === undefined) return Promise.reject(new Error(`unexpected ${key}`));
    return Promise.resolve(typeof answer === "function" ? answer() : answer);
  });
  return { fetchFn, calls };
}

/** Mounts the whole app at `path` over a real http layer and a fake socket. */
async function start(path: string, answers: Record<string, Answer>) {
  history.replaceState(null, "", path);
  const backend = server(answers);
  const socket = new FakeSocket();
  const leave = vi.fn<(url: string) => void>();
  const state = createAppState({
    api: createApi(backend.fetchFn),
    socket,
    schedule: immediately,
    leave,
  });
  const host = mount(() => <App state={state} />);
  await act();
  return { host, socket, state, leave, ...backend };
}

async function act(): Promise<void> {
  await settle();
  flush();
  await settle();
  flush();
}

const signInScreen = (host: HTMLElement): HTMLButtonElement | undefined =>
  [...host.querySelectorAll("button")].find((button) => button.textContent === "Sign in");

beforeEach(() => {
  history.replaceState(null, "", "/");
});

afterEach(() => {
  unmountAll();
  vi.useRealTimers();
});

describe("start-up", () => {
  it("asks /me before anything else, and opens nothing while it has not answered", async () => {
    const me = deferred<Response>();
    const { calls, socket, host } = await start("/", { "GET /api/v1/me": me.promise });

    expect(calls).toEqual(["GET /api/v1/me"]);
    expect(socket.subscriberCount("sessions")).toBe(0);
    expect(host.querySelector('nav[aria-label="Sessions"]')).toBeNull();
    expect(signInScreen(host)).toBeUndefined();
  });

  it("carries on as before in local mode, with no account anywhere", async () => {
    const { calls, socket, host, state } = await start("/settings", {
      "GET /api/v1/me": json(localMe),
      "GET /api/v1/health": json(health),
      "GET /api/v1/settings": json(settings),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
    });
    await vi.waitFor(() => {
      flush();
      expect(host.textContent).toContain("Home repo");
    });

    expect(calls[0]).toBe("GET /api/v1/me");
    expect(calls).toContain("GET /api/v1/health");
    expect(socket.subscriberCount("sessions")).toBe(1);
    expect(state.auth()).toEqual({ status: "local" });
    expect(host.textContent).toContain("Dry run: agents are simulated");
    expect(host.textContent).not.toContain("Sign out");
    expect(host.textContent).not.toContain("Access");
    expect(calls.some((call) => call.includes("/auth/") || call.includes("/tokens"))).toBe(false);
  });

  it("shows the account when signed in, and the dry-run banner like local mode", async () => {
    const { calls, socket, host } = await start("/", {
      "GET /api/v1/me": json(publicMe),
      "GET /api/v1/health": json(health),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
    });

    expect(calls[0]).toBe("GET /api/v1/me");
    expect(socket.subscriberCount("sessions")).toBe(1);
    const aside = find(host, "aside");
    const identity = find(aside, '[title="ada@example.org"]');
    expect(identity.textContent).toBe("Signed in as ada@example.org");
    expect(findByText(aside, "button", "Sign out")).toBeTruthy();
    expect(host.textContent).toContain("Dry run: agents are simulated");
  });

  it("asks to sign in on a 401, and asks for nothing else nor opens the socket", async () => {
    const { calls, socket, host, leave } = await start("/s/s1/changes?repo=api", {
      "GET /api/v1/me": signedOut(),
    });

    expect(calls).toEqual(["GET /api/v1/me"]);
    expect(socket.subscriberCount("sessions")).toBe(0);
    expect(socket.closed).toBe(true);
    expect(find(host, "h1").textContent).toBe("Calin");
    expect(host.textContent).toContain("Sign in to reach the agents on this server.");
    expect(host.textContent).not.toContain("Your session ended");
    // Not the app's views, and no "Connecting…" either: nothing is connecting.
    expect(host.querySelector('nav[aria-label="Sessions"]')).toBeNull();
    expect(host.querySelector("main")).toBeNull();
    expect(host.textContent).not.toContain("Connecting");

    const button = signInScreen(host);
    if (!button) throw new Error("no Sign in button");
    // No redirect of its own: only the button leaves.
    expect(leave).not.toHaveBeenCalled();
    await click(button);
    expect(leave).toHaveBeenCalledWith("/auth/login?next=%2Fs%2Fs1%2Fchanges%3Frepo%3Dapi");
  });

  it("says the server cannot be reached when /me fails, retries, and then carries on", async () => {
    let up = false;
    const { calls, socket, host } = await start("/", {
      "GET /api/v1/me": () => (up ? json(localMe) : new Response("Bad gateway", { status: 502 })),
      "GET /api/v1/health": json(health),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
    });

    expect(host.textContent).toContain("Cannot reach the server");
    expect(signInScreen(host)).toBeUndefined();
    expect(calls).toEqual(["GET /api/v1/me"]);
    expect(socket.subscriberCount("sessions")).toBe(0);

    up = true;
    await click(findByText(host, "button", "Retry now"));
    await act();
    expect(host.textContent).not.toContain("Cannot reach the server");
    expect(socket.subscriberCount("sessions")).toBe(1);
  });

  it("asks /me again on its own while the server cannot be reached", async () => {
    vi.useFakeTimers();
    const me = vi
      .fn<() => Promise<Response>>()
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValue(signedOut());
    const { host } = await start("/", { "GET /api/v1/me": () => me() });
    expect(host.textContent).toContain("Cannot reach the server");

    await vi.advanceTimersByTimeAsync(1000);
    await act();
    expect(me).toHaveBeenCalledTimes(2);
    expect(signInScreen(host)).toBeDefined();
  });

  it("treats a server that knows no /me as local mode", async () => {
    const { host, state } = await start("/", {
      "GET /api/v1/me": json({ error: "no such route" }, 404),
      "GET /api/v1/health": json(health),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
    });
    expect(state.auth()).toEqual({ status: "local" });
    expect(host.querySelector('nav[aria-label="Sessions"]')).not.toBeNull();
  });
});

describe("a session that ends", () => {
  const signedIn = (extra: Record<string, Answer> = {}): Record<string, Answer> => ({
    "GET /api/v1/me": json(publicMe),
    "GET /api/v1/health": json(health),
    "GET /api/v1/repos": json([]),
    "GET /api/v1/repo-sets": json([]),
    "GET /api/v1/workflows": json([]),
    ...extra,
  });

  it("ends on a 401 from any later call: the data goes, the socket stops", async () => {
    let ended = false;
    const { host, socket, state } = await start(
      "/settings",
      signedIn({
        "GET /api/v1/settings": () => (ended ? signedOut() : json(settings)),
      }),
    );
    socket.emit("sessions", {
      op: "session_upserted",
      session: {
        id: "s1",
        title: "Fix the build",
        prompt: "",
        status: "running",
        runner: "mock",
        permission_mode: "ask",
        workflow: null,
        branch: null,
        parent_session_id: null,
        root_session_id: "s1",
        head_seq: 0,
        created_at: 1,
        updated_at: 1,
      },
    });
    flush();
    expect(host.textContent).toContain("Fix the build");

    ended = true;
    state.loadSettings();
    await act();

    expect(host.textContent).toContain("Your session ended. Sign in again.");
    expect(host.textContent).not.toContain("Fix the build");
    expect(state.sessions()).toEqual([]);
    expect(socket.closed).toBe(true);
    expect(socket.subscriberCount("sessions")).toBe(0);
  });

  it("takes the reduced health shape as the end of the session, without breaking", async () => {
    const { host, state } = await start("/", {
      ...signedIn(),
      "GET /api/v1/health": json({ name: "calin", auth: "required" }),
    });
    expect(state.auth()).toEqual({ status: "signed_out", ended: true });
    expect(host.textContent).toContain("Your session ended. Sign in again.");
    expect(host.textContent).not.toContain("Dry run");
  });

  it("ends when the socket is refused, and stops reconnecting", async () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket implements WebSocketLike {
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onclose: ((event: CloseEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      constructor() {
        sockets.push(this);
      }
      send(): void {}
      close(): void {}
    }
    let ended = false;
    history.replaceState(null, "", "/");
    const backend = server({
      ...signedIn(),
      "GET /api/v1/me": () => (ended ? signedOut() : json(publicMe)),
    });
    const socket = createSocket({
      url: "ws://test/api/v1/ws",
      WebSocket: FakeWebSocket,
      backoffBaseMs: 100,
      backoffMaxMs: 100,
      random: () => 1,
    });
    const state = createAppState({
      api: createApi(backend.fetchFn),
      socket,
      schedule: immediately,
    });
    const host = mount(() => <App state={state} />);
    await act();
    expect(sockets).toHaveLength(1);
    sockets[0].onopen?.(new Event("open"));
    flush();

    // The server drops the socket, then refuses the upgrade: the browser sees two plain closes.
    ended = true;
    sockets[0].onclose?.(new CloseEvent("close"));
    await vi.advanceTimersByTimeAsync(100);
    expect(sockets).toHaveLength(2);
    sockets[1].onclose?.(new CloseEvent("close"));
    await act();

    expect(backend.calls.filter((call) => call === "GET /api/v1/me")).toHaveLength(2);
    expect(host.textContent).toContain("Your session ended. Sign in again.");
    await vi.advanceTimersByTimeAsync(60_000);
    socket.reconnectNow();
    expect(sockets).toHaveLength(2);
  });

  it("keeps unsaved text in the tab and says so", async () => {
    let ended = false;
    const { host, state } = await start(
      "/settings",
      signedIn({ "GET /api/v1/settings": () => (ended ? signedOut() : json(settings)) }),
    );
    await vi.waitFor(() => {
      flush();
      expect(host.textContent).toContain("Home repo");
    });
    state.setUnsavedWork("A file has unsaved changes.");
    flush();

    ended = true;
    state.loadSettings();
    await act();

    const alert = findByText(host, '[role="alert"]', "A file has unsaved changes.");
    expect(alert.textContent).toContain("your browser asks before they are lost");
    // Still mounted, out of sight and out of reach.
    const page = find(host, "main").parentElement;
    expect(page?.hidden).toBe(true);
    expect(page?.hasAttribute("inert")).toBe(true);

    await click(findByText(alert, "button", "Show unsaved changes"));
    expect(page?.hidden).toBe(false);
    expect(page?.hasAttribute("inert")).toBe(false);
    expect(host.textContent).toContain("Copy what you need, then sign in again.");
  });
});

describe("signing out", () => {
  it("ends the session on the server and shows the sign-in screen", async () => {
    const { host, calls, socket } = await start("/", {
      "GET /api/v1/me": json(publicMe),
      "GET /api/v1/health": json(health),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
      "POST /auth/logout": new Response(null, { status: 204 }),
    });
    await click(findByText(find(host, "aside"), "button", "Sign out"));
    await act();

    expect(calls).toContain("POST /auth/logout");
    expect(socket.closed).toBe(true);
    expect(host.textContent).toContain("Sign in to reach the agents on this server.");
    expect(host.textContent).not.toContain("Your session ended");
  });

  it("shows the server's reason when signing out fails", async () => {
    const { host } = await start("/", {
      "GET /api/v1/me": json(publicMe),
      "GET /api/v1/health": json(health),
      "GET /api/v1/repos": json([]),
      "GET /api/v1/repo-sets": json([]),
      "GET /api/v1/workflows": json([]),
      "POST /auth/logout": json({ error: "origin not allowed" }, 403),
    });
    await click(findByText(find(host, "aside"), "button", "Sign out"));
    await act();
    expect(host.textContent).toContain("Could not sign out: origin not allowed");
    expect(signInScreen(host)).toBeUndefined();
  });
});
