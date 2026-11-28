import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Health } from "../gen/Health";
import type { Me } from "../gen/Me";
import type { PushPreferences } from "../gen/PushPreferences";
import type { PushSubscriptionInfo } from "../gen/PushSubscriptionInfo";
import { AppContext, createAppState } from "../state/app";
import {
  badRequest,
  conflict,
  fakeApi,
  FakeDevice,
  FakeSocket,
  immediately,
  localMe,
  makeDeviceSubscription,
  settle,
} from "../test/fakes";
import { click, find, findByText, mount, unmountAll } from "../test/render";
import { InstallHint, NotificationSettings, PROBLEMS, UNAVAILABLE } from "./NotificationSettings";

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock"],
  github_problem: null,
};
const publicMe: Me = { mode: "public", identity: "ada@example.org", device_token: false };
const NOW = Date.now();
const MINUTE = 60_000;
const KEY = "server-key";

const device = (id: string, hash: string, patch: Partial<PushSubscriptionInfo> = {}) => ({
  id,
  label: `Device ${id}`,
  endpoint_sha256: hash,
  service: "fcm.googleapis.com",
  created_at: NOW - 60 * MINUTE,
  last_sent_at: null,
  ...patch,
});
const allOn: PushPreferences = { waiting: true, review: true, failed: true, merged: true };

const stops: Array<() => void> = [];
afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  unmountAll();
});

async function act(): Promise<void> {
  for (let turn = 0; turn < 4; turn += 1) {
    await settle();
    flush();
  }
}

async function open(
  options: {
    me?: Me;
    device?: FakeDevice;
    routes?: Partial<Api>;
    devices?: PushSubscriptionInfo[];
  } = {},
) {
  const fake = options.device ?? new FakeDevice();
  let listed = options.devices ?? [];
  // `fakeApi` wraps each route; these are the spies the tests look at.
  const spies = {
    getPushKey: vi.fn<Api["getPushKey"]>(() => Promise.resolve({ public_key: KEY })),
    listPushSubscriptions: vi.fn<Api["listPushSubscriptions"]>(() => Promise.resolve(listed)),
    createPushSubscription: vi.fn<Api["createPushSubscription"]>((request) => {
      const created = device("p-new", request.endpoint.split("/").pop() ?? "");
      listed = [created, ...listed];
      return Promise.resolve(created);
    }),
    deletePushSubscription: vi.fn<Api["deletePushSubscription"]>((id) => {
      listed = listed.filter((one) => one.id !== id);
      return Promise.resolve();
    }),
    getPushPreferences: vi.fn<Api["getPushPreferences"]>(() => Promise.resolve(allOn)),
    putPushPreferences: vi.fn<Api["putPushPreferences"]>((body) => Promise.resolve(body)),
    sendTestPush: vi.fn<Api["sendTestPush"]>(() => Promise.resolve()),
  };
  const api = fakeApi({
    me: () => Promise.resolve(options.me ?? publicMe),
    health: () => Promise.resolve(health),
    ...spies,
    ...options.routes,
  });
  const state = createAppState({
    api,
    socket: new FakeSocket(),
    schedule: immediately,
    device: fake,
  });
  stops.push(state.start());
  await settle();
  const host = mount(() => (
    <AppContext value={state}>
      <InstallHint />
      <NotificationSettings />
    </AppContext>
  ));
  await act();
  const section = () => findByText(host, "section", "Notifications");
  return { host, api: spies, device: fake, section };
}

const button = (root: ParentNode, name: string): HTMLButtonElement =>
  findByText<HTMLButtonElement>(root, "button", name);
const alertText = (root: ParentNode): string =>
  [...root.querySelectorAll('[role="alert"]')].map((one) => one.textContent).join(" | ");
const deviceRows = (root: ParentNode): HTMLElement[] => [
  ...root.querySelectorAll<HTMLElement>('ul[aria-label="Notified devices"] > li'),
];

async function press(element: HTMLElement): Promise<void> {
  await click(element);
  await act();
}

describe("Notifications availability", () => {
  it("says local mode cannot notify, and asks the server for nothing", async () => {
    const listPushSubscriptions = vi.fn<Api["listPushSubscriptions"]>();
    const { section } = await open({ me: localMe, routes: { listPushSubscriptions } });
    expect(section().textContent).toContain(UNAVAILABLE.local);
    expect(section().querySelector("button")).toBeNull();
    expect(listPushSubscriptions).not.toHaveBeenCalled();
  });

  it("asks iOS users to install the app first", async () => {
    const fake = new FakeDevice();
    fake.support = "needs-install";
    const { section } = await open({ device: fake });
    expect(section().textContent).toContain(UNAVAILABLE.needsInstall);
    expect(section().querySelector("button")).toBeNull();
  });

  it("says when the browser cannot do it, or the development server runs no worker", async () => {
    const unsupported = new FakeDevice();
    unsupported.support = "unsupported";
    expect((await open({ device: unsupported })).section().textContent).toContain(
      UNAVAILABLE.unsupported,
    );
    unmountAll();
    const dev = new FakeDevice();
    dev.support = "no-worker";
    expect((await open({ device: dev })).section().textContent).toContain(UNAVAILABLE.noWorker);
  });

  it("explains how to install, unless the app already runs from the home screen", async () => {
    const { host } = await open();
    const install = findByText(host, "section", "Install");
    expect(install.textContent).toContain("Add to Home Screen");
    expect(install.textContent).toContain("notifications only work once Calin is installed");
    unmountAll();
    const installed = new FakeDevice();
    installed.installed = true;
    const again = await open({ device: installed });
    expect(again.host.querySelector("h2")?.textContent).toBe("Notifications");
  });
});

describe("Notify this device", () => {
  it("asks, subscribes with the server's key, and registers the subscription", async () => {
    const { section, api, device: fake } = await open();
    expect(fake.asked).toBe(0);
    await press(button(section(), "Notify this device"));

    expect(fake.asked).toBe(1);
    expect(fake.subscribedWith).toEqual([KEY]);
    expect(api.createPushSubscription).toHaveBeenCalledWith({
      endpoint: "https://push.example/hash-new",
      p256dh: "p256",
      auth: "auth",
    });
    expect(section().textContent).toContain("This device is notified");
    expect(button(section(), "Stop notifying this device")).toBeDefined();
    const [row] = deviceRows(section());
    expect(row?.textContent).toContain("This device");
    expect(alertText(section())).toBe("");
  });

  it("explains how to allow notifications again when the browser blocks them", async () => {
    const fake = new FakeDevice();
    fake.answer = "denied";
    const { section, api } = await open({ device: fake });
    await press(button(section(), "Notify this device"));
    expect(alertText(section())).toBe(PROBLEMS.denied);
    expect(fake.subscribedWith).toEqual([]);
    expect(api.createPushSubscription).not.toHaveBeenCalled();
  });

  it("says the question was dismissed, and can ask again", async () => {
    const fake = new FakeDevice();
    fake.answer = "default";
    const { section } = await open({ device: fake });
    await press(button(section(), "Notify this device"));
    expect(alertText(section())).toBe(PROBLEMS.dismissed);
    expect(button(section(), "Notify this device").disabled).toBe(false);
  });

  it("says when the browser could not subscribe", async () => {
    const fake = new FakeDevice();
    fake.next = new Error("Registration failed - push service error");
    const { section, api } = await open({ device: fake });
    await press(button(section(), "Notify this device"));
    expect(alertText(section())).toBe(
      "This browser could not subscribe to notifications: Registration failed - push service error",
    );
    expect(api.createPushSubscription).not.toHaveBeenCalled();
  });

  it("shows the server's words when it refuses the subscription or its key", async () => {
    const createPushSubscription = vi
      .fn<Api["createPushSubscription"]>()
      .mockRejectedValue(badRequest("the endpoint must be https"));
    const { section } = await open({ routes: { createPushSubscription } });
    await press(button(section(), "Notify this device"));
    expect(alertText(section())).toBe(
      "The server did not accept this device: the endpoint must be https",
    );
    expect(section().textContent).not.toContain("This device is notified");

    unmountAll();
    const getPushKey = vi.fn<Api["getPushKey"]>().mockRejectedValue(conflict("local mode"));
    const keyless = await open({ routes: { getPushKey } });
    await press(button(keyless.section(), "Notify this device"));
    expect(alertText(keyless.section())).toContain("local mode");
  });

  it("recognizes this browser in the server's list by the hash of its endpoint", async () => {
    const fake = new FakeDevice();
    fake.current = makeDeviceSubscription("hash-b");
    fake.granted = "granted";
    const { section } = await open({
      device: fake,
      devices: [device("p1", "hash-a"), device("p2", "hash-b")],
    });
    expect(section().textContent).toContain("This device is notified");
    const rows = deviceRows(section());
    expect(rows.map((row) => row.dataset.mine !== undefined)).toEqual([false, true]);
  });

  it("reads the browser's answer when its prompt fails", async () => {
    const fake = new FakeDevice();
    fake.granted = "denied";
    vi.spyOn(fake, "requestPermission").mockRejectedValue(new Error("not in a user gesture"));
    const { section } = await open({ device: fake });
    await press(button(section(), "Notify this device"));
    expect(alertText(section())).toBe(PROBLEMS.denied);
    expect(fake.subscribedWith).toEqual([]);
  });

  it("explains a block it already knows of, before any click", async () => {
    const fake = new FakeDevice();
    fake.granted = "denied";
    const { section } = await open({ device: fake });
    expect(alertText(section())).toBe("");
    expect(section().textContent).toContain(PROBLEMS.denied);
    expect(button(section(), "Notify this device").disabled).toBe(false);
  });

  it("offers to notify when the browser cannot say what it holds", async () => {
    const fake = new FakeDevice();
    vi.spyOn(fake, "subscription").mockRejectedValue(new Error("no worker yet"));
    const { section } = await open({ device: fake, devices: [device("p1", "hash-a")] });
    expect(button(section(), "Notify this device")).toBeDefined();
    expect(deviceRows(section())).toHaveLength(1);
  });

  it("does not take a subscription the server does not list for a notified device", async () => {
    const fake = new FakeDevice();
    fake.current = makeDeviceSubscription("hash-z");
    const { section } = await open({ device: fake, devices: [device("p1", "hash-a")] });
    expect(button(section(), "Notify this device")).toBeDefined();
  });
});

describe("Stop notifying this device", () => {
  async function notified(unsubscribe = () => Promise.resolve(true), routes: Partial<Api> = {}) {
    const fake = new FakeDevice();
    fake.granted = "granted";
    const spy = vi.fn(unsubscribe);
    fake.current = makeDeviceSubscription("hash-b", KEY, spy);
    const opened = await open({
      device: fake,
      devices: [device("p1", "hash-a"), device("p2", "hash-b")],
      routes,
    });
    await press(button(opened.section(), "Stop notifying this device"));
    return { ...opened, unsubscribe: spy };
  }

  it("unsubscribes in the browser and on the server", async () => {
    const { section, api, unsubscribe } = await notified();
    expect(unsubscribe).toHaveBeenCalled();
    expect(api.deletePushSubscription).toHaveBeenCalledWith("p2");
    expect(section().textContent).toContain("This device is no longer notified.");
    expect(button(section(), "Notify this device")).toBeDefined();
    expect(deviceRows(section())).toHaveLength(1);
  });

  it("says so when the browser could not unsubscribe", async () => {
    const { section, api } = await notified(() => Promise.resolve(false));
    expect(api.deletePushSubscription).toHaveBeenCalledWith("p2");
    expect(alertText(section())).toContain("this browser could not unsubscribe");
  });

  it("says so when the server still lists it, in its words", async () => {
    const deletePushSubscription = vi
      .fn<Api["deletePushSubscription"]>()
      .mockRejectedValue(conflict("storage is read-only"));
    const { section } = await notified(undefined, { deletePushSubscription });
    expect(alertText(section())).toBe(
      "This browser unsubscribed, but the server still lists this device: storage is read-only",
    );
  });

  it("says nothing changed when both the browser and the server refuse", async () => {
    const deletePushSubscription = vi
      .fn<Api["deletePushSubscription"]>()
      .mockRejectedValue(conflict("storage is read-only"));
    const { section } = await notified(() => Promise.resolve(false), { deletePushSubscription });
    expect(alertText(section())).toBe(
      "Nothing changed: this browser could not unsubscribe, and the server refused to remove this device: storage is read-only",
    );
    expect(section().textContent).toContain("This device is notified");
  });

  it("stops from the device's own row in the list", async () => {
    const fake = new FakeDevice();
    fake.granted = "granted";
    fake.current = makeDeviceSubscription("hash-b");
    const { section, api } = await open({
      device: fake,
      devices: [device("p1", "hash-a"), device("p2", "hash-b")],
    });
    await press(find<HTMLButtonElement>(section(), 'button[aria-label="Remove device Device p2"]'));
    expect(api.deletePushSubscription).toHaveBeenCalledWith("p2");
    expect(section().textContent).toContain("This device is no longer notified.");
    expect(button(section(), "Notify this device")).toBeDefined();
  });
});

describe("Notified devices", () => {
  it("lists each device with its service, when it was added and last sent", async () => {
    const { section } = await open({
      devices: [
        device("p1", "hash-a", { label: "Pixel 9", last_sent_at: NOW - 5 * MINUTE }),
        device("p2", "hash-b", {
          label: "Firefox on Linux",
          service: "updates.push.services.mozilla.com",
        }),
      ],
    });
    const [pixel, firefox] = deviceRows(section());
    expect(pixel?.textContent).toContain("Pixel 9");
    expect(pixel?.textContent).toContain("fcm.googleapis.com");
    expect(pixel?.textContent).toContain("Added");
    expect(pixel?.textContent).toContain("Last sent");
    expect(pixel?.textContent).not.toContain("never");
    expect(firefox?.textContent).toContain("updates.push.services.mozilla.com");
    expect(firefox?.textContent).toContain("Last sent never");
  });

  it("says when no device is notified yet", async () => {
    const { section } = await open();
    expect(section().textContent).toContain("No device is notified yet.");
  });

  it("removes another device", async () => {
    const { section, api } = await open({
      devices: [device("p1", "hash-a"), device("p2", "hash-b")],
    });
    await press(find<HTMLButtonElement>(section(), 'button[aria-label="Remove device Device p1"]'));
    expect(api.deletePushSubscription).toHaveBeenCalledWith("p1");
    expect(deviceRows(section())).toHaveLength(1);
  });

  it("says when the list could not be read, and reads it again on Retry", async () => {
    let fail = true;
    const listPushSubscriptions = vi.fn<Api["listPushSubscriptions"]>(() =>
      fail ? Promise.reject(conflict("index is rebuilding")) : Promise.resolve([device("p1", "h")]),
    );
    const { section } = await open({ routes: { listPushSubscriptions } });
    expect(alertText(section())).toBe(
      "The notified devices could not be loaded: index is rebuildingRetry",
    );
    expect(section().textContent).toContain("Checking this device…");
    fail = false;
    await press(button(findByText(section(), "section", "Notified devices"), "Retry"));
    expect(alertText(section())).toBe("");
    expect(deviceRows(section())).toHaveLength(1);
  });

  it("keeps a device whose removal the server refused, with its reason", async () => {
    const deletePushSubscription = vi
      .fn<Api["deletePushSubscription"]>()
      .mockRejectedValue(conflict("try later"));
    const { section } = await open({
      devices: [device("p1", "hash-a")],
      routes: { deletePushSubscription },
    });
    await press(find<HTMLButtonElement>(section(), 'button[aria-label="Remove device Device p1"]'));
    expect(deviceRows(section())).toHaveLength(1);
    expect(alertText(section())).toBe("Could not remove it: try later");
  });
});

describe("Notify me when", () => {
  const toggle = (root: ParentNode, label: string): HTMLInputElement => {
    const row = findByText<HTMLLabelElement>(root, "label", label);
    return find<HTMLInputElement>(row, 'input[role="switch"]');
  };

  it("shows the four choices as the server has them, each with what it means", async () => {
    const getPushPreferences = vi
      .fn<Api["getPushPreferences"]>()
      .mockResolvedValue({ ...allOn, merged: false });
    const { section } = await open({ routes: { getPushPreferences } });
    const labels = ["Waiting on you", "Ready for review", "Failed", "All pull requests merged"];
    expect(labels.map((label) => toggle(section(), label).checked)).toEqual([
      true,
      true,
      true,
      false,
    ]);
    const described = toggle(section(), "Ready for review").getAttribute("aria-describedby") ?? "";
    expect(document.getElementById(described)?.textContent).toContain("waits for your review");
  });

  it("saves a change at once", async () => {
    const { section, api } = await open();
    await press(toggle(section(), "Ready for review"));
    expect(api.putPushPreferences).toHaveBeenCalledWith({ ...allOn, review: false });
    expect(toggle(section(), "Ready for review").checked).toBe(false);
  });

  it("puts a switch back, with the server's reason, when the save is refused", async () => {
    const putPushPreferences = vi
      .fn<Api["putPushPreferences"]>()
      .mockRejectedValue(badRequest("preferences are locked"));
    const { section } = await open({ routes: { putPushPreferences } });
    await press(toggle(section(), "Failed"));
    expect(toggle(section(), "Failed").checked).toBe(true);
    expect(alertText(section())).toBe("Not saved: preferences are locked");
  });

  it("says when the choices could not be read, and reads them again on Retry", async () => {
    let fail = true;
    const getPushPreferences = vi.fn<Api["getPushPreferences"]>(() =>
      fail ? Promise.reject(conflict("database is locked")) : Promise.resolve(allOn),
    );
    const { section } = await open({ routes: { getPushPreferences } });
    expect(alertText(section())).toBe(
      "The notification choices could not be loaded: database is lockedRetry",
    );
    fail = false;
    await press(button(findByText(section(), "fieldset", "Notify me when"), "Retry"));
    expect(alertText(section())).toBe("");
    expect(toggle(section(), "Failed").checked).toBe(true);
  });
});

describe("Send a test notification", () => {
  it("asks the server to send one, and says it may take a moment", async () => {
    const { section, api } = await open();
    expect(section().textContent).toContain("It can take a few seconds to arrive.");
    await press(button(section(), "Send a test notification"));
    expect(api.sendTestPush).toHaveBeenCalledTimes(1);
    const statuses = [...section().querySelectorAll('[role="status"]')].map(
      (one) => one.textContent,
    );
    expect(statuses).toContain("Sent to every notified device.");
  });

  it("shows the server's reason when it cannot", async () => {
    const sendTestPush = vi
      .fn<Api["sendTestPush"]>()
      .mockRejectedValue(conflict("no device is subscribed"));
    const { section } = await open({ routes: { sendTestPush } });
    await press(button(section(), "Send a test notification"));
    expect(alertText(section())).toBe("Could not send it: no device is subscribed");
  });
});

describe("a change of the server's key", () => {
  it("subscribes again with the new key when the section opens, and drops the old entry", async () => {
    const fake = new FakeDevice();
    fake.granted = "granted";
    const unsubscribe = vi.fn(() => Promise.resolve(true));
    fake.current = makeDeviceSubscription("hash-old", "old-key", unsubscribe);
    fake.next = makeDeviceSubscription("hash-renewed", KEY);
    const { section, api } = await open({ device: fake, devices: [device("p-old", "hash-old")] });

    expect(unsubscribe).toHaveBeenCalled();
    expect(fake.subscribedWith).toEqual([KEY]);
    expect(api.createPushSubscription).toHaveBeenCalledWith(
      expect.objectContaining({ endpoint: "https://push.example/hash-renewed" }),
    );
    expect(api.deletePushSubscription).toHaveBeenCalledWith("p-old");
    expect(section().textContent).toContain("This device is notified");
  });

  it("says so when it cannot renew, and keeps what the browser still holds", async () => {
    const fake = new FakeDevice();
    fake.granted = "granted";
    fake.current = makeDeviceSubscription("hash-old", "old-key");
    fake.next = new Error("push service unreachable");
    const { section, api } = await open({ device: fake, devices: [device("p-old", "hash-old")] });

    expect(fake.subscribedWith).toEqual([KEY]);
    expect(api.createPushSubscription).not.toHaveBeenCalled();
    expect(alertText(section())).toBe(
      "Could not renew this device after the server's key changed: push service unreachable",
    );
    expect(section().textContent).toContain("This device is notified");
  });

  it("leaves a subscription made with the current key alone", async () => {
    const fake = new FakeDevice();
    fake.granted = "granted";
    fake.current = makeDeviceSubscription("hash-b", KEY);
    const { api } = await open({ device: fake, devices: [device("p2", "hash-b")] });
    expect(fake.subscribedWith).toEqual([]);
    expect(api.createPushSubscription).not.toHaveBeenCalled();
  });
});
