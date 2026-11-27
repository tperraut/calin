// What the page and the service worker say to each other, and where the worker keeps its state.
// Shared by both sides; it holds no DOM or worker-only type, so either program can import it.

/** The script's address: stable across builds, so the browser sees a new build as an update. */
export const WORKER_URL = "/sw.js";

/** One cache per build; the version is a hash of the build (see `vite.config.ts`). */
export const SHELL_CACHE_PREFIX = "calin-shell-";

/** Survives builds: what the worker needs to renew a push subscription on its own. */
export const STATE_CACHE = "calin-state";
/** The VAPID key this browser last subscribed with, base64url. Never fetched over the network. */
export const PUSH_KEY_ENTRY = "/__calin/push-key";
/** Present when the worker renewed the subscription and the server has not been told yet. */
export const RECONCILE_ENTRY = "/__calin/push-reconcile";

/** Worker to page. */
export type WorkerMessage =
  /** A notification was tapped: show `path`. */
  | { type: "navigate"; path: string }
  /** A push arrived for the session this page shows; it was not shown as a notification. */
  | { type: "refresh"; path: string; sessionId: string | null }
  /** The browser renewed the push subscription: tell the server (see `RECONCILE_ENTRY`). */
  | { type: "subscription-changed" }
  /**
   * Which path do you show? Answered on the message's port with a `WhereAnswer`.
   * `WindowClient.url` does not follow `history.pushState` in Chromium, so it cannot tell.
   */
  | { type: "where" };

export type WhereAnswer = { path: string };

/** Page to worker: the user chose to reload into the waiting build. */
export type PageMessage = { type: "activate" };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function isWorkerMessage(value: unknown): value is WorkerMessage {
  if (!isRecord(value)) return false;
  switch (value.type) {
    case "navigate":
      return typeof value.path === "string";
    case "refresh":
      return (
        typeof value.path === "string" &&
        (value.sessionId === null || typeof value.sessionId === "string")
      );
    case "subscription-changed":
    case "where":
      return true;
    default:
      return false;
  }
}

export function isWhereAnswer(value: unknown): value is WhereAnswer {
  return isRecord(value) && typeof value.path === "string";
}

export function isPageMessage(value: unknown): value is PageMessage {
  return isRecord(value) && value.type === "activate";
}

/**
 * `path` when it is a path on this origin, else `/`: a notification's target
 * comes from the server, and must never become a way to open another site.
 */
export function appPath(path: unknown): string {
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) return "/";
  if (path.includes("\\")) return "/";
  return path;
}

/** Base64url (padded or not) to bytes, as `applicationServerKey` wants it. */
export function base64UrlToBytes(text: string): Uint8Array<ArrayBuffer> {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/").replace(/=+$/, "");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

/** Bytes to unpadded base64url, the form the contract uses for keys. */
export function bytesToBase64Url(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const byte of view) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
