import { describe, expect, it, vi } from "vite-plus/test";
import { appPath, base64UrlToBytes, bytesToBase64Url, isWorkerMessage } from "../sw/protocol";
import { isIos, sameKey, sha256Hex, withTimeout } from "./device";

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
