import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { App } from "./App";
import type { Api } from "./api/http";
import type { Health } from "./gen/Health";
import type { Me } from "./gen/Me";
import type { WorkerBridge } from "./lib/serviceWorker";
import { createAppState } from "./state/app";
import type { WorkerMessage } from "./sw/protocol";
import {
  fakeApi,
  FakeDevice,
  FakeSocket,
  immediately,
  makeDeviceSubscription,
  settle,
  unreachable,
} from "./test/fakes";
import { click, find, findByText, mount, unmountAll } from "./test/render";

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock"],
  github_problem: null,
};
const publicMe: Me = { mode: "public", identity: "ada@example.org", device_token: false };

/** The page's side of a service worker, driven by the test. */
class FakeWorkerBridge implements WorkerBridge {
  private readonly ready = new Set<() => void>();
  private readonly listeners = new Set<(message: WorkerMessage) => void>();
  applied = 0;
  onUpdateReady(listener: () => void): () => void {
    this.ready.add(listener);
    return () => this.ready.delete(listener);
  }
  applyUpdate(): void {
    this.applied += 1;
  }
  onMessage(listener: (message: WorkerMessage) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  newBuildInstalled(): void {
    for (const listener of this.ready) listener();
  }
  say(message: WorkerMessage): void {
    for (const listener of this.listeners) listener(message);
  }
}

async function act(): Promise<void> {
  for (let turn = 0; turn < 4; turn += 1) {
    await settle();
    flush();
  }
}

function setOnline(online: boolean): void {
  Object.defineProperty(navigator, "onLine", { configurable: true, value: online });
}

async function start(
  path: string,
  options: { routes?: Partial<Api>; worker?: FakeWorkerBridge; device?: FakeDevice } = {},
) {
  history.replaceState(null, "", path);
  const socket = new FakeSocket();
  const api = fakeApi({
    health: () => Promise.resolve(health),
    listRepos: () => Promise.resolve([]),
    ...options.routes,
  });
  const state = createAppState({
    api,
    socket,
    schedule: immediately,
    worker: options.worker,
    device: options.device,
  });
  const host = mount(() => <App state={state} />);
  await act();
  socket.setState("open");
  await act();
  return { host, socket, state };
}

afterEach(() => {
  unmountAll();
  Reflect.deleteProperty(navigator, "onLine");
});

describe("offline", () => {
  it("says the device may be offline, and asks again as soon as the browser is back online", async () => {
    setOnline(false);
    const me = vi.fn<Api["me"]>().mockRejectedValueOnce(unreachable()).mockResolvedValue({
      mode: "local",
      identity: null,
      device_token: false,
    });
    const { host } = await start("/", { routes: { me } });

    const alert = find(host, '[role="alert"]');
    expect(alert.textContent).toContain("Cannot reach the server");
    expect(alert.textContent).toContain("You are offline, or the server is unreachable.");
    expect(me).toHaveBeenCalledTimes(1);

    setOnline(true);
    window.dispatchEvent(new Event("online"));
    await act();

    expect(me).toHaveBeenCalledTimes(2);
    expect(host.textContent).not.toContain("Cannot reach the server");
  });

  it("keeps its usual words when the browser says it is online", async () => {
    setOnline(true);
    const me = vi.fn<Api["me"]>().mockRejectedValue(unreachable());
    const { host } = await start("/", { routes: { me } });
    const alert = find(host, '[role="alert"]');
    expect(alert.textContent).toContain("Check that calin serve is running.");
    expect(alert.textContent).not.toContain("You are offline");
  });
});

describe("a new version", () => {
  it("is announced without interrupting, and reloading lets it in", async () => {
    const worker = new FakeWorkerBridge();
    const { host } = await start("/", { worker });
    expect(host.textContent).not.toContain("A new version is ready.");

    worker.newBuildInstalled();
    await act();
    const notice = findByText(host, '[role="status"]', "A new version is ready.");
    expect(worker.applied).toBe(0);
    // The page under it stays usable.
    expect(host.querySelector("textarea")).not.toBeNull();

    await click(findByText(notice, "button", "Reload"));
    expect(worker.applied).toBe(1);
  });
});

describe("the worker's messages", () => {
  it("open the session of a tapped notification", async () => {
    const worker = new FakeWorkerBridge();
    await start("/", { worker });
    worker.say({ type: "navigate", path: "/settings" });
    // The settings page's code is fetched on its first visit (router.ts).
    await vi.waitFor(() => expect(location.pathname).toBe("/settings"));
  });

  it("make sure the socket is live when a push arrives for the open session", async () => {
    const worker = new FakeWorkerBridge();
    const { socket } = await start("/", { worker });
    worker.say({ type: "refresh", path: "/s/s1", sessionId: "s1" });
    expect(socket.reconnects).toBe(1);
  });

  it("report a subscription the worker renewed to the server", async () => {
    const worker = new FakeWorkerBridge();
    const device = new FakeDevice();
    device.current = makeDeviceSubscription("hash-renewed");
    const createPushSubscription = vi.fn<Api["createPushSubscription"]>(() =>
      Promise.reject(new Error("unused")),
    );
    await start("/", {
      worker,
      device,
      routes: { me: () => Promise.resolve(publicMe), createPushSubscription },
    });
    expect(createPushSubscription).not.toHaveBeenCalled();

    device.renewed = true;
    worker.say({ type: "subscription-changed" });
    await act();
    expect(createPushSubscription).toHaveBeenCalledWith(device.current.request);
  });

  it("are reported on the next start when no page was open", async () => {
    const device = new FakeDevice();
    device.renewed = true;
    device.current = makeDeviceSubscription("hash-renewed");
    const createPushSubscription = vi.fn<Api["createPushSubscription"]>(() =>
      Promise.reject(new Error("unused")),
    );
    await start("/", {
      device,
      routes: { me: () => Promise.resolve(publicMe), createPushSubscription },
    });
    expect(createPushSubscription).toHaveBeenCalledTimes(1);
    expect(device.renewed).toBe(false);
  });
});
