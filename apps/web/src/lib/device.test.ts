import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import {
  appPath,
  base64UrlToBytes,
  bytesToBase64Url,
  isWorkerMessage,
  PUSH_KEY_ENTRY,
  RECONCILE_ENTRY,
  STATE_CACHE,
} from "../sw/protocol";
import { browserDevice, isIos, sameKey, sha256Hex, withTimeout, type Permission } from "./device";

const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";
const MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15";
const PIXEL =
  "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36";

describe("this device", () => {
  it("hashes an endpoint as the server lists it: lowercase hex SHA-256", async () => {
    expect(await sha256Hex("abc")).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  it("compares keys whatever their padding", () => {
    expect(sameKey("BAbc", "BAbc==")).toBe(true);
    expect(sameKey("BAbc", "BAbd")).toBe(false);
  });

  it("round-trips base64url keys", () => {
    const bytes = new Uint8Array([4, 250, 255, 0, 62, 63]);
    const text = bytesToBase64Url(bytes);
    expect(text).not.toMatch(/[+/=]/);
    expect(base64UrlToBytes(text)).toEqual(bytes);
  });

  it("tells iPhones and iPads, including iPads that say they are a Mac", () => {
    expect(isIos(IPHONE, 5)).toBe(true);
    expect(isIos(MAC, 5)).toBe(true);
    expect(isIos(MAC, 0)).toBe(false);
    expect(isIos(PIXEL, 5)).toBe(false);
  });

  it("only follows paths on this origin", () => {
    expect(appPath("/s/s1")).toBe("/s/s1");
    for (const path of ["//evil.example", "https://evil.example", "/\\evil.example", 3, null]) {
      expect(appPath(path)).toBe("/");
    }
  });

  it("recognizes the worker's messages", () => {
    expect(isWorkerMessage({ type: "refresh", path: "/s/1", sessionId: null })).toBe(true);
    expect(isWorkerMessage({ type: "refresh", path: "/s/1" })).toBe(false);
    expect(isWorkerMessage({ type: "other" })).toBe(false);
  });

  it("gives up on a push service that never answers", async () => {
    vi.useFakeTimers();
    try {
      const never = new Promise<string>(() => {});
      const waiting = withTimeout(never, 30_000, "the browser's push service did not answer");
      vi.advanceTimersByTime(30_000);
      await expect(waiting).rejects.toThrow("the browser's push service did not answer");
      await expect(withTimeout(Promise.resolve("ok"), 10, "late")).resolves.toBe("ok");
    } finally {
      vi.useRealTimers();
    }
  });
});

type StandInSubscription = {
  endpoint: string;
  options: { applicationServerKey: ArrayBuffer | null };
  toJSON(): { endpoint: string; keys?: { p256dh: string; auth: string } };
  unsubscribe(): Promise<boolean>;
};

function standInSubscription(
  endpoint: string,
  extra: {
    key?: Uint8Array<ArrayBuffer>;
    keys?: { p256dh: string; auth: string };
    unsubscribe?: () => Promise<boolean>;
  } = {},
): StandInSubscription {
  return {
    endpoint,
    options: { applicationServerKey: extra.key?.buffer ?? null },
    toJSON: () => ({ endpoint, keys: extra.keys }),
    unsubscribe: extra.unsubscribe ?? (() => Promise.resolve(true)),
  };
}

/** Stand-ins for the browser globals `browserDevice` reads; what it does with them is recorded. */
function standInBrowser(
  options: {
    userAgent?: string;
    touchPoints?: number;
    displayStandalone?: boolean;
    safariStandalone?: boolean;
    serviceWorker?: boolean;
    permission?: Permission;
    answer?: Permission;
    existing?: StandInSubscription | null;
    made?: StandInSubscription;
  } = {},
) {
  const cached = new Map<string, string>();
  const subscribedWith: { userVisibleOnly: boolean; applicationServerKey: Uint8Array }[] = [];
  const pushManager = {
    getSubscription: () => Promise.resolve(options.existing ?? null),
    subscribe: (init: { userVisibleOnly: boolean; applicationServerKey: Uint8Array }) => {
      subscribedWith.push(init);
      return Promise.resolve(options.made ?? standInSubscription("https://push.example/made"));
    },
  };
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query === "(display-mode: standalone)" && options.displayStandalone === true,
  }));
  vi.stubGlobal("navigator", {
    userAgent: options.userAgent ?? PIXEL,
    maxTouchPoints: options.touchPoints ?? 0,
    ...(options.safariStandalone === undefined ? {} : { standalone: options.safariStandalone }),
    ...(options.serviceWorker === false
      ? {}
      : { serviceWorker: { ready: Promise.resolve({ pushManager }) } }),
  });
  vi.stubGlobal("PushManager", class {});
  vi.stubGlobal("Notification", {
    permission: options.permission ?? "default",
    requestPermission: () => Promise.resolve(options.answer ?? "granted"),
  });
  vi.stubGlobal("caches", {
    open: (name: string) =>
      Promise.resolve({
        put: async (path: string, response: Response) => {
          cached.set(`${name} ${path}`, await response.text());
        },
        delete: (path: string) => Promise.resolve(cached.delete(`${name} ${path}`)),
      }),
  });
  return { cached, subscribedWith };
}

describe("the browser as a device", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("offers push where the browser has it, and says why not elsewhere", () => {
    standInBrowser();
    expect(browserDevice(true).pushSupport()).toBe("supported");
    expect(browserDevice(false).pushSupport()).toBe("no-worker");

    standInBrowser({ serviceWorker: false });
    expect(browserDevice(true).pushSupport()).toBe("unsupported");
  });

  it("asks iOS to install first, and offers push once it runs from the home screen", () => {
    standInBrowser({ userAgent: IPHONE, touchPoints: 5 });
    expect(browserDevice(true).standalone()).toBe(false);
    expect(browserDevice(true).pushSupport()).toBe("needs-install");

    standInBrowser({ userAgent: IPHONE, touchPoints: 5, displayStandalone: true });
    expect(browserDevice(true).standalone()).toBe(true);
    expect(browserDevice(true).pushSupport()).toBe("supported");

    standInBrowser({ userAgent: IPHONE, touchPoints: 5, safariStandalone: true });
    expect(browserDevice(true).standalone()).toBe(true);
    expect(browserDevice(true).pushSupport()).toBe("supported");
  });

  it("reads and asks for the notification permission", async () => {
    standInBrowser({ permission: "denied", answer: "granted" });
    const device = browserDevice(true);
    expect(device.permission()).toBe("denied");
    expect(await device.requestPermission()).toBe("granted");
  });

  it("has no subscription until the browser holds one", async () => {
    standInBrowser({ existing: null });
    expect(await browserDevice(true).subscription()).toBeNull();
  });

  it("describes the subscription it holds as the server is told about it", async () => {
    const key = new Uint8Array([4, 250, 255, 0]);
    standInBrowser({
      existing: standInSubscription("abc", {
        key,
        keys: { p256dh: "P256", auth: "AUTH" },
        unsubscribe: () => Promise.reject(new Error("gone")),
      }),
    });
    const held = await browserDevice(true).subscription();
    expect(held?.request).toEqual({ endpoint: "abc", p256dh: "P256", auth: "AUTH" });
    expect(held?.endpointSha256).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
    expect(held?.publicKey).toBe(bytesToBase64Url(key));
    // A browser that fails to unsubscribe says so rather than throwing.
    expect(await held?.unsubscribe()).toBe(false);
  });

  it("describes a subscription that does not say its keys", async () => {
    standInBrowser({ existing: standInSubscription("abc") });
    const held = await browserDevice(true).subscription();
    expect(held?.request).toEqual({ endpoint: "abc", p256dh: "", auth: "" });
    expect(held?.publicKey).toBeNull();
    expect(await held?.unsubscribe()).toBe(true);
  });

  it("subscribes with the server's key as bytes, and remembers it for the worker", async () => {
    const key = bytesToBase64Url(new Uint8Array([4, 1, 2, 3, 250]));
    const browser = standInBrowser({
      made: standInSubscription("https://push.example/new", { keys: { p256dh: "P", auth: "A" } }),
    });
    const made = await browserDevice(true).subscribe(key);
    expect(browser.subscribedWith).toEqual([
      { userVisibleOnly: true, applicationServerKey: base64UrlToBytes(key) },
    ]);
    expect(browser.cached.get(`${STATE_CACHE} ${PUSH_KEY_ENTRY}`)).toBe(key);
    expect(made.request).toEqual({ endpoint: "https://push.example/new", p256dh: "P", auth: "A" });
  });

  it("takes the worker's renewal note once", async () => {
    const browser = standInBrowser();
    browser.cached.set(`${STATE_CACHE} ${RECONCILE_ENTRY}`, "");
    const device = browserDevice(true);
    expect(await device.takeRenewal()).toBe(true);
    expect(await device.takeRenewal()).toBe(false);
  });
});
