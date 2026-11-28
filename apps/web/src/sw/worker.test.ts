import { describe, expect, it, vi } from "vite-plus/test";
import type { PushPayload } from "../gen/PushPayload";
import { PUSH_KEY_ENTRY, RECONCILE_ENTRY, STATE_CACHE, type WorkerMessage } from "./protocol";
import {
  BADGE,
  createWorker,
  GENERIC_NOTIFICATION,
  ICON,
  parsePushPayload,
  shellCacheName,
  showsPath,
  staleCaches,
  type CacheLike,
  type CacheStorageLike,
  type NotificationSpec,
  type WindowLike,
  type WorkerDeps,
} from "./worker";

const ORIGIN = "https://calin.example.com";

class FakeCache implements CacheLike {
  readonly entries = new Map<string, Response>();
  private readonly server: (url: string) => Response;
  constructor(server: (url: string) => Response) {
    this.server = server;
  }
  private key(request: Request | string): string {
    return new URL(typeof request === "string" ? request : request.url, ORIGIN).pathname;
  }
  match(request: Request | string): Promise<Response | undefined> {
    return Promise.resolve(this.entries.get(this.key(request))?.clone());
  }
  put(request: Request | string, response: Response): Promise<void> {
    this.entries.set(this.key(request), response);
    return Promise.resolve();
  }
  addAll(requests: string[]): Promise<void> {
    for (const url of requests) this.entries.set(url, this.server(url));
    return Promise.resolve();
  }
}

class FakeCaches implements CacheStorageLike {
  readonly caches = new Map<string, FakeCache>();
  open(name: string): Promise<FakeCache> {
    const cache = this.caches.get(name) ?? new FakeCache(() => new Response("file"));
    this.caches.set(name, cache);
    return Promise.resolve(cache);
  }
  keys(): Promise<string[]> {
    return Promise.resolve([...this.caches.keys()]);
  }
  delete(name: string): Promise<boolean> {
    return Promise.resolve(this.caches.delete(name));
  }
}

class FakeWindow implements WindowLike {
  readonly messages: WorkerMessage[] = [];
  focusCalls = 0;
  readonly url: string;
  readonly focused: boolean;
  asked = 0;
  readonly visibilityState: string;
  constructor(url: string, focused = false, visibilityState = "visible") {
    this.url = url;
    this.focused = focused;
    this.visibilityState = visibilityState;
  }
  path(): Promise<string> {
    this.asked += 1;
    return Promise.resolve(new URL(this.url).pathname);
  }
  focus(): Promise<unknown> {
    this.focusCalls += 1;
    return Promise.resolve(this);
  }
  postMessage(message: WorkerMessage): void {
    this.messages.push(message);
  }
}

function setup(patch: Partial<WorkerDeps> = {}, windows: FakeWindow[] = []) {
  const caches = new FakeCaches();
  const shown: Array<{ title: string; spec: NotificationSpec }> = [];
  const spies = {
    fetch: vi.fn<WorkerDeps["fetch"]>(() => Promise.resolve(new Response("network"))),
    openWindow: vi.fn<WorkerDeps["openWindow"]>(() => Promise.resolve(undefined)),
    claim: vi.fn<WorkerDeps["claim"]>(() => Promise.resolve()),
    skipWaiting: vi.fn<WorkerDeps["skipWaiting"]>(() => Promise.resolve()),
    subscribe: vi.fn<WorkerDeps["subscribe"]>(() => Promise.resolve({})),
  };
  const deps: WorkerDeps = {
    version: "v2",
    precache: ["/index.html", "/assets/index-a.js"],
    origin: ORIGIN,
    caches,
    windows: () => Promise.resolve(windows),
    showNotification(title, spec) {
      shown.push({ title, spec });
      return Promise.resolve();
    },
    ...spies,
    ...patch,
  };
  return { worker: createWorker(deps), deps: spies, caches, shown };
}

const payload = (patch: Partial<PushPayload> = {}): string =>
  JSON.stringify({
    kind: "waiting",
    title: "Fix the login form",
    body: "Asks to run npm test",
    path: "/s/s1",
    session_id: "s1",
    ...patch,
  });

/** A page load; `respond` is only asked for the routes `route` chose. */
const page = (path: string): Request => new Request(ORIGIN + path);

describe("cache versioning", () => {
  it("names one cache per build and finds every other build's shell cache", () => {
    expect(shellCacheName("v2")).toBe("calin-shell-v2");
    expect(
      staleCaches(["calin-shell-v1", "calin-shell-v2", STATE_CACHE, "someone-else"], "v2"),
    ).toEqual(["calin-shell-v1"]);
  });

  it("pre-caches the shell on install, and waits instead of taking over", async () => {
    const { worker, caches, deps } = setup();
    await worker.install();
    const cache = caches.caches.get("calin-shell-v2");
    expect([...(cache?.entries.keys() ?? [])]).toEqual(["/index.html", "/assets/index-a.js"]);
    expect(deps.skipWaiting).not.toHaveBeenCalled();
  });

  it("deletes the caches of earlier builds on activate, and keeps its state", async () => {
    const { worker, caches, deps } = setup();
    await caches.open("calin-shell-v1");
    await caches.open(STATE_CACHE);
    await worker.install();
    await worker.activate();
    expect([...caches.caches.keys()].sort()).toEqual(["calin-shell-v2", STATE_CACHE]);
    expect(deps.claim).toHaveBeenCalled();
  });

  it("lets the waiting build in only when the page asks", async () => {
    const { worker, deps } = setup();
    await worker.activateWaiting();
    expect(deps.skipWaiting).toHaveBeenCalledTimes(1);
  });
});

describe("answering requests", () => {
  it("takes page loads from the network when it answers", async () => {
    const { worker } = setup();
    await worker.install();
    const response = await worker.respond("navigate", page("/s/s1"));
    expect(await response.text()).toBe("network");
  });

  it("opens the cached shell when the network fails or the proxy has no server behind it", async () => {
    const down = setup({ fetch: () => Promise.reject(new TypeError("Failed to fetch")) });
    await down.worker.install();
    expect(await (await down.worker.respond("navigate", page("/s/s1"))).text()).toBe("file");

    const proxy = setup({
      fetch: () => Promise.resolve(new Response("bad gateway", { status: 502 })),
    });
    await proxy.worker.install();
    expect(await (await proxy.worker.respond("navigate", page("/"))).text()).toBe("file");

    const broken = setup({
      fetch: () => Promise.resolve(new Response("server error", { status: 500 })),
    });
    await broken.worker.install();
    expect(await (await broken.worker.respond("navigate", page("/"))).text()).toBe("file");
  });

  it("hands back any answer below 500 as it came, a 404 included", async () => {
    const { worker } = setup({
      fetch: () => Promise.resolve(new Response("not here", { status: 404 })),
    });
    await worker.install();
    expect(await (await worker.respond("navigate", page("/nope"))).text()).toBe("not here");
  });

  it("hands back the proxy's answer, or the network's failure, when no shell is cached", async () => {
    const down = setup({ fetch: () => Promise.reject(new TypeError("Failed to fetch")) });
    await expect(down.worker.respond("navigate", page("/s/s1"))).rejects.toThrow("Failed to fetch");

    const proxy = setup({
      fetch: () => Promise.resolve(new Response("bad gateway", { status: 502 })),
    });
    const response = await proxy.worker.respond("navigate", page("/"));
    expect(response.status).toBe(502);
    expect(await response.text()).toBe("bad gateway");
  });

  it("serves the shell from the cache without asking the network", async () => {
    const { worker, deps } = setup();
    await worker.install();
    await worker.respond("precached", new Request(`${ORIGIN}/assets/index-a.js`));
    expect(deps.fetch).not.toHaveBeenCalled();
  });

  it("caches a lazy file the first time, and never the server's page in its place", async () => {
    const js = new Response("chunk", { headers: { "content-type": "text/javascript" } });
    const html = new Response("<!doctype html>", { headers: { "content-type": "text/html" } });
    const fetch = vi
      .fn<WorkerDeps["fetch"]>()
      .mockResolvedValueOnce(js)
      .mockResolvedValueOnce(html);
    const { worker, caches } = setup({ fetch });
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/sql-1.js`));
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/gone-2.js`));
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/sql-1.js`));
    expect(fetch).toHaveBeenCalledTimes(2);
    expect([...(caches.caches.get("calin-shell-v2")?.entries.keys() ?? [])]).toEqual([
      "/assets/sql-1.js",
    ]);
  });

  it("does not cache a failed answer", async () => {
    const fetch = () => Promise.resolve(new Response("nope", { status: 404 }));
    const { worker, caches } = setup({ fetch });
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/x-1.js`));
    expect(caches.caches.get("calin-shell-v2")?.entries.size).toBe(0);
  });

  it("caches a lazy file without a content type, and never one another origin answered", async () => {
    const opaque = new Response("x");
    Object.defineProperty(opaque, "type", { value: "opaque" });
    const fetch = vi
      .fn<WorkerDeps["fetch"]>()
      .mockResolvedValueOnce(new Response("bare"))
      .mockResolvedValueOnce(opaque);
    const { worker, caches } = setup({ fetch });
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/bare-1.js`));
    await worker.respond("on-use", new Request(`${ORIGIN}/assets/far-1.js`));
    expect([...(caches.caches.get("calin-shell-v2")?.entries.keys() ?? [])]).toEqual([
      "/assets/bare-1.js",
    ]);
  });

  it("fetches a shell file missing from the cache without storing it", async () => {
    const { worker, caches, deps } = setup();
    const response = await worker.respond("precached", new Request(`${ORIGIN}/assets/new-1.js`));
    expect(await response.text()).toBe("network");
    expect(deps.fetch).toHaveBeenCalledTimes(1);
    expect(caches.caches.get("calin-shell-v2")?.entries.size).toBe(0);
  });

  it("routes the API and sign-in past itself, so nothing of them is ever cached", () => {
    const { worker } = setup();
    for (const url of ["/api/v1/sessions", "/auth/login", "/api/v1/ws"]) {
      expect(worker.route(new Request(ORIGIN + url))).toBe("pass");
    }
    expect(worker.route(new Request(`${ORIGIN}/assets/index-a.js`, { method: "POST" }))).toBe(
      "pass",
    );
  });
});

describe("a push", () => {
  it("shows the session's title and reason, tagged with the session, with the app's icons", async () => {
    const { worker, shown } = setup();
    await worker.push(payload());
    expect(shown).toEqual([
      {
        title: "Fix the login form",
        spec: {
          body: "Asks to run npm test",
          tag: "s1",
          icon: ICON,
          badge: BADGE,
          data: { path: "/s/s1" },
        },
      },
    ]);
  });

  it("tags every notification of a session alike, so a newer one replaces the older", async () => {
    const { worker, shown } = setup();
    await worker.push(payload({ kind: "waiting" }));
    await worker.push(payload({ kind: "review", body: "Ready for review" }));
    await worker.push(payload({ session_id: "s2", path: "/s/s2" }));
    expect(shown.map((one) => one.spec.tag)).toEqual(["s1", "s1", "s2"]);
  });

  it("shows a test under its own tag", async () => {
    const { worker, shown } = setup();
    await worker.push(payload({ kind: "test", title: "Calin", path: "/", session_id: null }));
    expect(shown[0]?.spec.tag).toBe("calin-test");
  });

  it("still shows something for a missing or malformed payload", async () => {
    const { worker, shown } = setup();
    for (const text of [
      undefined,
      "",
      "not json",
      "[]",
      JSON.stringify({ kind: "nope", title: "x", body: "y", path: "/" }),
      JSON.stringify({ kind: "waiting", title: 3, body: "y", path: "/" }),
    ]) {
      await worker.push(text);
    }
    expect(shown).toHaveLength(6);
    for (const one of shown) {
      expect(one.title).toBe(GENERIC_NOTIFICATION.title);
      expect(one.spec.data.path).toBe("/");
    }
  });

  it("never sends a tap to another site", async () => {
    const { worker, shown } = setup();
    await worker.push(payload({ path: "//evil.example/s/s1" }));
    await worker.push(payload({ path: "https://evil.example/" }));
    expect(shown.map((one) => one.spec.data.path)).toEqual(["/", "/"]);
  });

  it("tells a focused window showing that session instead of notifying", async () => {
    const viewer = new FakeWindow(`${ORIGIN}/s/s1/changes`, true);
    const { worker, shown } = setup({}, [viewer]);
    await worker.push(payload());
    expect(shown).toEqual([]);
    expect(viewer.messages).toEqual([{ type: "refresh", path: "/s/s1", sessionId: "s1" }]);
  });

  it("notifies when the window shows another session, is not focused, or is hidden", async () => {
    const elsewhere = new FakeWindow(`${ORIGIN}/s/s10`, true);
    const behind = new FakeWindow(`${ORIGIN}/s/s1`, false);
    const hidden = new FakeWindow(`${ORIGIN}/s/s1`, true, "hidden");
    const { worker, shown } = setup({}, [elsewhere, behind, hidden]);
    await worker.push(payload());
    expect(shown).toHaveLength(1);
    expect([elsewhere, behind, hidden].flatMap((one) => one.messages)).toEqual([]);
    // Only a window the user is looking at is asked where it is.
    expect([elsewhere.asked, behind.asked, hidden.asked]).toEqual([1, 0, 0]);
  });

  it("tells a focused window on the home page about a push for the home page", async () => {
    const home = new FakeWindow(`${ORIGIN}/`, true);
    const { worker, shown } = setup({}, [home]);
    await worker.push(payload({ path: "/" }));
    expect(shown).toEqual([]);
    expect(home.messages).toEqual([{ type: "refresh", path: "/", sessionId: "s1" }]);
  });

  it("matches a window to a path on its own panels only", () => {
    expect(showsPath("/", "/")).toBe(true);
    expect(showsPath("/s/s1", "/")).toBe(false);
    expect(showsPath("/s/s1", "/s/s1")).toBe(true);
    expect(showsPath("/s/s1/changes", "/s/s1")).toBe(true);
    expect(showsPath("/s/s10", "/s/s1")).toBe(false);
  });

  it("refuses every payload that is not a well-formed PushPayload", () => {
    const base = { kind: "waiting", title: "t", body: "b", path: "/s/s1", session_id: "s1" };
    const refused = [
      undefined,
      "",
      "not json",
      "null",
      "42",
      '"text"',
      "true",
      "[]",
      "{}",
      JSON.stringify({ ...base, kind: "nope" }),
      JSON.stringify({ ...base, kind: 1 }),
      JSON.stringify({ ...base, title: 3 }),
      JSON.stringify({ ...base, title: undefined }),
      JSON.stringify({ ...base, body: null }),
      JSON.stringify({ ...base, path: 7 }),
      JSON.stringify({ ...base, path: undefined }),
      JSON.stringify({ ...base, session_id: 5 }),
      JSON.stringify({ ...base, session_id: {} }),
      // Falsy is not absent: only a missing or null session means none.
      JSON.stringify({ ...base, session_id: false }),
      JSON.stringify({ ...base, session_id: 0 }),
    ];
    for (const text of refused) expect(parsePushPayload(text), String(text)).toBeUndefined();
  });

  it("keeps only the contract's fields, and takes a null session as none", () => {
    const extra = JSON.stringify({
      kind: "merged",
      title: "t",
      body: "b",
      path: "https://evil.example/",
      session_id: null,
      more: 1,
    });
    expect(parsePushPayload(extra)).toEqual({
      kind: "merged",
      title: "t",
      body: "b",
      path: "https://evil.example/",
      session_id: null,
    });
  });

  it("reads payloads as the contract writes them", () => {
    expect(parsePushPayload(payload())).toEqual({
      kind: "waiting",
      title: "Fix the login form",
      body: "Asks to run npm test",
      path: "/s/s1",
      session_id: "s1",
    });
    const withoutSession = JSON.stringify({ kind: "test", title: "Calin", body: "b", path: "/" });
    expect(parsePushPayload(withoutSession)?.session_id).toBeNull();
  });
});

describe("tapping a notification", () => {
  const tapped = (path: unknown = "/s/s1") => ({ data: { path }, close: vi.fn() });

  it("focuses an open window of the app and has it show the session", async () => {
    const open = new FakeWindow(`${ORIGIN}/`);
    const { worker, deps } = setup({}, [open]);
    const notification = tapped();
    await worker.notificationClick(notification);
    expect(notification.close).toHaveBeenCalled();
    expect(open.focusCalls).toBe(1);
    expect(open.messages).toEqual([{ type: "navigate", path: "/s/s1" }]);
    expect(deps.openWindow).not.toHaveBeenCalled();
  });

  it("still has the window show the session when the browser refuses to focus it", async () => {
    const open = new FakeWindow(`${ORIGIN}/`);
    open.focus = () => Promise.reject(new Error("Not allowed to focus a window."));
    const { worker } = setup({}, [open]);
    await worker.notificationClick(tapped());
    expect(open.messages).toEqual([{ type: "navigate", path: "/s/s1" }]);
  });

  it("prefers the focused window when there are several", async () => {
    const first = new FakeWindow(`${ORIGIN}/`);
    const focused = new FakeWindow(`${ORIGIN}/settings`, true);
    const { worker } = setup({}, [first, focused]);
    await worker.notificationClick(tapped());
    expect(focused.messages).toHaveLength(1);
    expect(first.messages).toHaveLength(0);
  });

  it("opens a window at the session when none is open", async () => {
    const { worker, deps } = setup();
    await worker.notificationClick(tapped());
    expect(deps.openWindow).toHaveBeenCalledWith("/s/s1");
  });

  it("opens the app's home for a notification without a usable path", async () => {
    const { worker, deps } = setup();
    await worker.notificationClick({ data: null, close: () => {} });
    await worker.notificationClick(tapped("https://evil.example/"));
    expect(deps.openWindow.mock.calls).toEqual([["/"], ["/"]]);
  });
});

describe("a renewed subscription", () => {
  async function remember(caches: FakeCaches, key: string): Promise<void> {
    await (await caches.open(STATE_CACHE)).put(PUSH_KEY_ENTRY, new Response(key));
  }

  it("subscribes again with the key it last used, and has an open page tell the server", async () => {
    const open = new FakeWindow(`${ORIGIN}/`);
    const { worker, deps, caches } = setup({}, [open]);
    await remember(caches, "AQID");
    await worker.subscriptionChange(false);
    expect(deps.subscribe.mock.calls[0]?.[0]).toEqual(new Uint8Array([1, 2, 3]));
    expect(open.messages).toEqual([{ type: "subscription-changed" }]);
  });

  it("leaves a flag for the next start when no page is open", async () => {
    const { worker, caches } = setup();
    await remember(caches, "AQID");
    await worker.subscriptionChange(false);
    expect(await (await caches.open(STATE_CACHE)).match(RECONCILE_ENTRY)).toBeDefined();
  });

  it("does not subscribe without a key it used, and still leaves the flag", async () => {
    const open = new FakeWindow(`${ORIGIN}/`);
    const { worker, deps, caches } = setup({}, [open]);
    await worker.subscriptionChange(false);
    expect(deps.subscribe).not.toHaveBeenCalled();
    expect(await (await caches.open(STATE_CACHE)).match(RECONCILE_ENTRY)).toBeDefined();
    expect(open.messages).toEqual([{ type: "subscription-changed" }]);
  });

  it("does not subscribe again when the browser already did", async () => {
    const { worker, deps, caches } = setup();
    await remember(caches, "AQID");
    await worker.subscriptionChange(true);
    expect(deps.subscribe).not.toHaveBeenCalled();
    expect(await (await caches.open(STATE_CACHE)).match(RECONCILE_ENTRY)).toBeDefined();
  });
});
