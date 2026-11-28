// What this browser can do for notifications, and its push subscription. The only place
// that touches `Notification`, `PushManager` and the worker's state cache from the page.
import type { PushSubscriptionRequest } from "../gen/PushSubscriptionRequest";
import {
  base64UrlToBytes,
  bytesToBase64Url,
  PUSH_KEY_ENTRY,
  RECONCILE_ENTRY,
  STATE_CACHE,
} from "../sw/protocol";

/**
 * Why notifications can or cannot be offered here:
 * `needs-install` is iOS outside the home screen, where Safari has no Web Push;
 * `no-worker` is a development server, which registers no service worker.
 */
export type PushSupport = "supported" | "unsupported" | "needs-install" | "no-worker";

export type Permission = "default" | "denied" | "granted";

/** This browser's push subscription, as the server is told about it. */
export type DeviceSubscription = {
  request: PushSubscriptionRequest;
  /** Lowercase hex SHA-256 of the endpoint, to find it in the server's list. */
  endpointSha256: string;
  /** The key it was made with, base64url; null when the browser does not say. */
  publicKey: string | null;
  /** Resolves false when the browser could not unsubscribe. */
  unsubscribe(): Promise<boolean>;
};

export interface Device {
  /** True when running from the home screen (or as an installed app). */
  standalone(): boolean;
  pushSupport(): PushSupport;
  permission(): Permission;
  /** Must be called straight from a click: browsers only ask in a user gesture. */
  requestPermission(): Promise<Permission>;
  subscription(): Promise<DeviceSubscription | null>;
  /** Subscribes with the server's key, and remembers it for the worker to renew with. */
  subscribe(publicKey: string): Promise<DeviceSubscription>;
  /** True, once, when the worker renewed the subscription and the server was not told. */
  takeRenewal(): Promise<boolean>;
}

/** How long the browser's push service may take to subscribe before the user is told. */
const SUBSCRIBE_TIMEOUT_MS = 30_000;

/** `promise`, or a rejection with `message` once `ms` passed: a push service can hang forever. */
export function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error instanceof Error ? error : new Error(String(error)));
      },
    );
  });
}

/** Lowercase hex SHA-256 of `text`, as `PushSubscriptionInfo.endpoint_sha256` is. */
export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Two base64url keys are the same key, padding aside. */
export function sameKey(a: string, b: string): boolean {
  return a.replace(/=+$/, "") === b.replace(/=+$/, "");
}

/** iPhone and iPad, including iPads that say they are a Mac. */
export function isIos(userAgent: string, maxTouchPoints: number): boolean {
  return /iPhone|iPad|iPod/.test(userAgent) || (/Macintosh/.test(userAgent) && maxTouchPoints > 1);
}

/** A device with no notification support at all: the default in tests and outside a browser. */
export const NO_DEVICE: Device = {
  standalone: () => false,
  pushSupport: () => "unsupported",
  permission: () => "default",
  requestPermission: () => Promise.resolve("default"),
  subscription: () => Promise.resolve(null),
  subscribe: () => Promise.reject(new Error("This browser cannot receive notifications.")),
  takeRenewal: () => Promise.resolve(false),
};

async function describe(subscription: PushSubscription): Promise<DeviceSubscription> {
  const json = subscription.toJSON();
  const key = subscription.options.applicationServerKey;
  return {
    request: {
      endpoint: subscription.endpoint,
      p256dh: json.keys?.p256dh ?? "",
      auth: json.keys?.auth ?? "",
    },
    endpointSha256: await sha256Hex(subscription.endpoint),
    publicKey: key ? bytesToBase64Url(key) : null,
    unsubscribe: () => subscription.unsubscribe().catch(() => false),
  };
}

/** The real browser. `workerEnabled` is false under the development server. */
class BrowserDevice implements Device {
  private readonly workerEnabled: boolean;

  constructor(workerEnabled: boolean) {
    this.workerEnabled = workerEnabled;
  }

  standalone(): boolean {
    return (
      window.matchMedia("(display-mode: standalone)").matches ||
      // Safari's own flag, from before display-mode.
      ("standalone" in navigator && navigator.standalone === true)
    );
  }

  pushSupport(): PushSupport {
    if (isIos(navigator.userAgent, navigator.maxTouchPoints) && !this.standalone()) {
      return "needs-install";
    }
    const capable =
      "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
    if (!capable) return "unsupported";
    return this.workerEnabled ? "supported" : "no-worker";
  }

  permission(): Permission {
    return Notification.permission;
  }

  requestPermission(): Promise<Permission> {
    return Notification.requestPermission();
  }

  async subscription(): Promise<DeviceSubscription | null> {
    const current = await (await this.registration()).pushManager.getSubscription();
    return current ? describe(current) : null;
  }

  async subscribe(publicKey: string): Promise<DeviceSubscription> {
    const manager = (await this.registration()).pushManager;
    const created = await withTimeout(
      manager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: base64UrlToBytes(publicKey),
      }),
      SUBSCRIBE_TIMEOUT_MS,
      "the browser's push service did not answer",
    );
    await (await this.stateCache()).put(PUSH_KEY_ENTRY, new Response(publicKey));
    return describe(created);
  }

  async takeRenewal(): Promise<boolean> {
    return (await this.stateCache()).delete(RECONCILE_ENTRY);
  }

  private registration(): Promise<ServiceWorkerRegistration> {
    return navigator.serviceWorker.ready;
  }

  private stateCache(): Promise<Cache> {
    return caches.open(STATE_CACHE);
  }
}

export function browserDevice(workerEnabled: boolean): Device {
  return new BrowserDevice(workerEnabled);
}
