// The service worker's behavior, with the browser handed in so the tests can play it.
// `sw.ts` wires it to the real `self`. Remote-access spec, section 6.
import type { PushKind } from "../gen/PushKind";
import type { PushPayload } from "../gen/PushPayload";
import {
  appPath,
  base64UrlToBytes,
  PUSH_KEY_ENTRY,
  RECONCILE_ENTRY,
  SHELL_CACHE_PREFIX,
  STATE_CACHE,
  type WorkerMessage,
} from "./protocol";
import { routeRequest, type RequestLike, type Route } from "./routing";

export interface CacheLike {
  match(request: Request | string): Promise<Response | undefined>;
  put(request: Request | string, response: Response): Promise<void>;
  addAll(requests: string[]): Promise<void>;
}

export interface CacheStorageLike {
  open(name: string): Promise<CacheLike>;
  keys(): Promise<string[]>;
  delete(name: string): Promise<boolean>;
}

export interface WindowLike {
  /** The path the window shows now, asked of the page itself. */
  path(): Promise<string>;
  focused: boolean;
  visibilityState: string;
  focus(): Promise<unknown>;
  postMessage(message: WorkerMessage): void;
}

export type NotificationSpec = {
  body: string;
  /** One per session: a newer notification replaces the older one. */
  tag: string;
  icon: string;
  badge: string;
  data: { path: string };
};

export type WorkerDeps = {
  /** A hash of the build; names this build's cache. */
  version: string;
  /** Root-relative URLs of the shell (see `precacheList`). */
  precache: readonly string[];
  origin: string;
  caches: CacheStorageLike;
  fetch(request: Request): Promise<Response>;
  /** Every window of the app on this origin, controlled by this worker or not. */
  windows(): Promise<readonly WindowLike[]>;
  openWindow(url: string): Promise<unknown>;
  claim(): Promise<void>;
  skipWaiting(): Promise<void>;
  showNotification(title: string, spec: NotificationSpec): Promise<void>;
  /** Subscribes this browser again; resolves once the push service answered. */
  subscribe(applicationServerKey: Uint8Array<ArrayBuffer>): Promise<unknown>;
};

export const ICON = "/icons/icon-192.png";
export const BADGE = "/icons/badge-96.png";

/** Shown when a push carries nothing readable: a push that shows nothing is penalized. */
export const GENERIC_NOTIFICATION = {
  title: "Calin needs you",
  body: "Open Calin to see what is waiting.",
  path: "/",
  tag: "calin",
} as const;

const TEST_TAG = "calin-test";

const KINDS: ReadonlySet<string> = new Set<PushKind>([
  "waiting",
  "review",
  "failed",
  "merged",
  "test",
]);

function isKind(value: unknown): value is PushKind {
  return typeof value === "string" && KINDS.has(value);
}

/** The payload when it is a well-formed `PushPayload`, else `undefined`. */
export function parsePushPayload(text: string | undefined): PushPayload | undefined {
  if (text === undefined || text === "") return undefined;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  const { kind, title, body, path } = value as Record<string, unknown>;
  const sessionId: unknown = (value as Record<string, unknown>).session_id ?? null;
  if (!isKind(kind) || typeof title !== "string" || typeof body !== "string") return undefined;
  if (typeof path !== "string" || (sessionId !== null && typeof sessionId !== "string")) {
    return undefined;
  }
  return { kind, title, body, path, session_id: sessionId };
}

/** True when a window at `pathname` shows `path`, on any of its panels (`/s/<id>/<panel>`). */
export function showsPath(pathname: string, path: string): boolean {
  if (path === "/") return pathname === "/";
  return pathname === path || pathname.startsWith(`${path}/`);
}

export function shellCacheName(version: string): string {
  return SHELL_CACHE_PREFIX + version;
}

/** Caches of earlier builds: every shell cache but this build's. The state cache is kept. */
export function staleCaches(names: readonly string[], version: string): string[] {
  const current = shellCacheName(version);
  return names.filter((name) => name.startsWith(SHELL_CACHE_PREFIX) && name !== current);
}

/** Only what the server sent as the file itself; its single-page fallback is HTML. */
function cacheable(response: Response): boolean {
  if (!response.ok || (response.type !== "basic" && response.type !== "default")) return false;
  return !(response.headers.get("content-type") ?? "").startsWith("text/html");
}

export function createWorker(deps: WorkerDeps) {
  const cacheName = shellCacheName(deps.version);
  const precached = new Set(deps.precache);

  async function fromShell(url: string): Promise<Response | undefined> {
    const cache = await deps.caches.open(cacheName);
    return cache.match(url);
  }

  async function navigate(request: Request): Promise<Response> {
    try {
      const response = await deps.fetch(request);
      // A proxy in front of a stopped server answers 502: the shell still opens, and says so.
      if (response.status < 500) return response;
      return (await fromShell("/index.html")) ?? response;
    } catch (error) {
      const shell = await fromShell("/index.html");
      if (shell) return shell;
      throw error;
    }
  }

  async function cacheFirst(request: Request, keep: boolean): Promise<Response> {
    const cache = await deps.caches.open(cacheName);
    const hit = await cache.match(request);
    if (hit) return hit;
    const response = await deps.fetch(request);
    if (keep && cacheable(response)) await cache.put(request, response.clone());
    return response;
  }

  async function writeState(entry: string, value: string): Promise<void> {
    const cache = await deps.caches.open(STATE_CACHE);
    await cache.put(entry, new Response(value));
  }

  async function readState(entry: string): Promise<string | undefined> {
    const cache = await deps.caches.open(STATE_CACHE);
    const hit = await cache.match(entry);
    return hit ? hit.text() : undefined;
  }

  return {
    cacheName,

    /** Pre-caches this build's shell. Waits to be activated: never takes over a running page. */
    async install(): Promise<void> {
      const cache = await deps.caches.open(cacheName);
      await cache.addAll([...deps.precache]);
    },

    async activate(): Promise<void> {
      const names = await deps.caches.keys();
      await Promise.all(staleCaches(names, deps.version).map((name) => deps.caches.delete(name)));
      await deps.claim();
    },

    route(request: RequestLike): Route {
      return routeRequest(request, deps.origin, precached);
    },

    /** The answer for a request `route` did not `pass`. */
    respond(route: Exclude<Route, "pass">, request: Request): Promise<Response> {
      if (route === "navigate") return navigate(request);
      return cacheFirst(request, route === "on-use");
    },

    /** Called when the page asks for the waiting build. */
    activateWaiting(): Promise<void> {
      return deps.skipWaiting();
    },

    async push(text: string | undefined): Promise<void> {
      const payload = parsePushPayload(text);
      if (!payload) {
        const { title, body, path, tag } = GENERIC_NOTIFICATION;
        await deps.showNotification(title, { body, tag, icon: ICON, badge: BADGE, data: { path } });
        return;
      }
      const path = appPath(payload.path);
      if (payload.session_id !== null) {
        const watching = (await deps.windows()).filter(
          (one) => one.focused && one.visibilityState === "visible",
        );
        const paths = await Promise.all(watching.map((one) => one.path()));
        const viewer = watching.find((_, index) => showsPath(paths[index] ?? "", path));
        if (viewer) {
          viewer.postMessage({ type: "refresh", path, sessionId: payload.session_id });
          return;
        }
      }
      await deps.showNotification(payload.title, {
        body: payload.body,
        tag: payload.session_id ?? TEST_TAG,
        icon: ICON,
        badge: BADGE,
        data: { path },
      });
    },

    async notificationClick(notification: { data: unknown; close(): void }): Promise<void> {
      notification.close();
      const data = notification.data;
      const path = appPath(
        typeof data === "object" && data !== null && "path" in data ? data.path : "/",
      );
      const windows = await deps.windows();
      const target = windows.find((one) => one.focused) ?? windows[0];
      if (target) {
        // A browser may refuse focus (no user activation left); the window still navigates.
        await target.focus().catch(() => undefined);
        target.postMessage({ type: "navigate", path });
        return;
      }
      await deps.openWindow(path);
    },

    /**
     * The push service replaced or dropped the subscription. The browser
     * subscribes again with the key it last used, and the page tells the
     * server: now if one is open, else the next time the app starts.
     */
    async subscriptionChange(renewed: boolean): Promise<void> {
      if (!renewed) {
        const key = await readState(PUSH_KEY_ENTRY);
        if (key !== undefined) await deps.subscribe(base64UrlToBytes(key));
      }
      await writeState(RECONCILE_ENTRY, "1");
      for (const one of await deps.windows()) one.postMessage({ type: "subscription-changed" });
    },
  };
}

export type CalinWorker = ReturnType<typeof createWorker>;
