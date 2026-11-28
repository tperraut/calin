import { describe, expect, it, vi } from "vite-plus/test";
import type { PageMessage, WhereAnswer } from "../sw/protocol";
import { settle } from "../test/fakes";
import {
  registerWorker,
  type ContainerLike,
  type RegistrationLike,
  type WorkerLike,
} from "./serviceWorker";

class FakeWorker implements WorkerLike {
  readonly sent: PageMessage[] = [];
  private readonly listeners: Array<() => void> = [];
  state: string;
  constructor(state: string) {
    this.state = state;
  }
  postMessage(message: PageMessage): void {
    this.sent.push(message);
  }
  addEventListener(_type: "statechange", listener: () => void): void {
    this.listeners.push(listener);
  }
  become(state: string): void {
    this.state = state;
    for (const listener of this.listeners) listener();
  }
}

class FakeRegistration implements RegistrationLike {
  installing: FakeWorker | null = null;
  waiting: FakeWorker | null = null;
  updates = 0;
  private readonly found: Array<() => void> = [];
  addEventListener(_type: "updatefound", listener: () => void): void {
    this.found.push(listener);
  }
  update(): Promise<unknown> {
    this.updates += 1;
    return Promise.resolve();
  }
  /** The browser found a newer script and starts installing it. */
  install(worker: FakeWorker | null): void {
    this.installing = worker;
    for (const listener of this.found) listener();
  }
}

type Port = { postMessage(answer: WhereAnswer): void };
type MessageListener = (event: { data: unknown; ports: readonly Port[] }) => void;

class FakeContainer implements ContainerLike {
  controller: unknown = null;
  registered: string[] = [];
  started = false;
  private readonly controllerListeners: Array<() => void> = [];
  private readonly messageListeners: MessageListener[] = [];
  readonly registration = new FakeRegistration();
  register(url: string): Promise<RegistrationLike> {
    this.registered.push(url);
    return Promise.resolve(this.registration);
  }
  addEventListener(type: "controllerchange" | "message", listener: MessageListener): void {
    if (type === "controllerchange") {
      this.controllerListeners.push(() => listener({ data: null, ports: [] }));
    } else this.messageListeners.push(listener);
  }
  startMessages(): void {
    this.started = true;
  }
  changeController(): void {
    this.controller = {};
    for (const listener of this.controllerListeners) listener();
  }
  deliver(data: unknown, ports: readonly Port[] = []): void {
    for (const listener of this.messageListeners) listener({ data, ports });
  }
}

async function setup(container = new FakeContainer()) {
  const reload = vi.fn();
  let visible: (() => void) | undefined;
  const bridge = registerWorker(container, {
    reload,
    currentPath: () => "/s/s1/changes",
    onVisible: (check) => {
      visible = check;
    },
  });
  await settle();
  const ready = vi.fn();
  bridge.onUpdateReady(ready);
  return { bridge, container, reload, ready, visible: () => visible?.() };
}

describe("the service worker registration", () => {
  it("registers the worker at the site root", async () => {
    const { container } = await setup();
    expect(container.registered).toEqual(["/sw.js"]);
    expect(container.started).toBe(true);
  });

  it("does not offer an update for the first install, nor reload when it takes control", async () => {
    const { container, ready, reload } = await setup();
    const first = new FakeWorker("installing");
    container.registration.install(first);
    first.become("installed");
    first.become("activated");
    container.changeController();
    expect(ready).not.toHaveBeenCalled();
    expect(reload).not.toHaveBeenCalled();
  });

  it("offers a newer build once installed, and lets it in only on request", async () => {
    const container = new FakeContainer();
    container.controller = {};
    const { bridge, ready, reload } = await setup(container);
    const next = new FakeWorker("installing");
    container.registration.install(next);
    expect(ready).not.toHaveBeenCalled();
    next.become("installed");
    expect(ready).toHaveBeenCalledTimes(1);
    expect(next.sent).toEqual([]);

    bridge.applyUpdate();
    expect(next.sent).toEqual([{ type: "activate" }]);
    expect(reload).not.toHaveBeenCalled();
    container.changeController();
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("offers a build that was already waiting when the page opened", async () => {
    const container = new FakeContainer();
    container.controller = {};
    container.registration.waiting = new FakeWorker("installed");
    const { ready } = await setup(container);
    expect(ready).toHaveBeenCalledTimes(1);
  });

  it("looks for a newer build when the page comes back to the foreground", async () => {
    const { container, visible } = await setup();
    visible();
    expect(container.registration.updates).toBe(1);
  });

  it("passes on the worker's messages and ignores anything else", async () => {
    const { bridge, container } = await setup();
    const heard = vi.fn();
    bridge.onMessage(heard);
    container.deliver({ type: "navigate", path: "/s/s1" });
    container.deliver({ type: "navigate" });
    container.deliver("hello");
    expect(heard.mock.calls).toEqual([[{ type: "navigate", path: "/s/s1" }]]);
  });

  it("tells the worker which path the page shows, which its client URL does not follow", async () => {
    const { bridge, container } = await setup();
    const heard = vi.fn();
    bridge.onMessage(heard);
    const answers: WhereAnswer[] = [];
    container.deliver({ type: "where" }, [{ postMessage: (answer) => answers.push(answer) }]);
    expect(answers).toEqual([{ path: "/s/s1/changes" }]);
    expect(heard).not.toHaveBeenCalled();
  });

  it("answers with the page's own path by default, and asks for no port", async () => {
    const container = new FakeContainer();
    registerWorker(container, { reload: () => {} });
    const answers: WhereAnswer[] = [];
    container.deliver({ type: "where" }, [{ postMessage: (answer) => answers.push(answer) }]);
    expect(answers).toEqual([{ path: location.pathname }]);
    expect(() => container.deliver({ type: "where" })).not.toThrow();
  });

  it("lets nothing in, nor reloads, when no build waits", async () => {
    const container = new FakeContainer();
    container.controller = {};
    const { bridge, reload, ready } = await setup(container);
    // The browser found an update but has nothing installing any more.
    container.registration.install(null);
    bridge.applyUpdate();
    container.changeController();
    expect(ready).not.toHaveBeenCalled();
    expect(reload).not.toHaveBeenCalled();
  });

  it("keeps looking on later visits when an update check fails", async () => {
    const { container, visible } = await setup();
    let checks = 0;
    container.registration.update = () => {
      checks += 1;
      return Promise.reject(new Error("offline"));
    };
    visible();
    await settle();
    visible();
    await settle();
    expect(checks).toBe(2);
  });

  it("warns when the worker cannot be registered, and carries on", async () => {
    const container = new FakeContainer();
    container.register = () => Promise.reject(new Error("insecure origin"));
    const warn = vi.fn();
    const bridge = registerWorker(container, { reload: () => {}, warn });
    await settle();
    expect(warn).toHaveBeenCalledWith(
      "calin: the service worker could not be registered: Error: insecure origin",
    );
    registerWorker(container, { reload: () => {} });
    await settle();
    expect(() => bridge.applyUpdate()).not.toThrow();
  });
});
