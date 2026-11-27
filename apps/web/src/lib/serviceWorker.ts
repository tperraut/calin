// The page's side of the service worker: registering it, noticing a newer build waiting
// behind it, and hearing what it says. Remote-access spec, section 6.
import {
  isWorkerMessage,
  WORKER_URL,
  type PageMessage,
  type WhereAnswer,
  type WorkerMessage,
} from "../sw/protocol";

/** The parts of a `ServiceWorker` used here. */
export interface WorkerLike {
  state: string;
  postMessage(message: PageMessage): void;
  addEventListener(type: "statechange", listener: () => void): void;
}

export interface RegistrationLike {
  installing: WorkerLike | null;
  waiting: WorkerLike | null;
  addEventListener(type: "updatefound", listener: () => void): void;
  update(): Promise<unknown>;
}

export interface ContainerLike {
  controller: unknown;
  register(url: string, options: { scope: string }): Promise<RegistrationLike>;
  addEventListener(type: "controllerchange", listener: () => void): void;
  addEventListener(
    type: "message",
    listener: (event: {
      data: unknown;
      ports: ReadonlyArray<{ postMessage(answer: WhereAnswer): void }>;
    }) => void,
  ): void;
  startMessages(): void;
}

export interface WorkerBridge {
  /** Called once a newer build is installed and waits for the page to let it in. */
  onUpdateReady(listener: () => void): () => void;
  /** Lets the waiting build in; the page reloads once it controls it. */
  applyUpdate(): void;
  onMessage(listener: (message: WorkerMessage) => void): () => void;
}

/**
 * Registers the worker. A build installed while a page is open waits: it is
 * let in only by `applyUpdate`, so nothing changes under the user mid-session.
 */
export function registerWorker(
  container: ContainerLike,
  options: {
    reload: () => void;
    /** Where to look for a newer build when the page comes back to the foreground. */
    onVisible?: (check: () => void) => void;
    warn?: (message: string) => void;
    /** The path this page shows; the worker asks before it notifies (see `WhereAnswer`). */
    currentPath?: () => string;
  },
): WorkerBridge {
  const currentPath = options.currentPath ?? (() => location.pathname);
  const readyListeners = new Set<() => void>();
  const messageListeners = new Set<(message: WorkerMessage) => void>();
  let waiting: WorkerLike | undefined;
  let reloading = false;

  function ready(worker: WorkerLike): void {
    waiting = worker;
    for (const listener of readyListeners) listener();
  }

  /** A worker that finished installing waits only if another one already runs this page. */
  function watch(worker: WorkerLike): void {
    const check = (): void => {
      if (worker.state === "installed" && container.controller) ready(worker);
    };
    check();
    worker.addEventListener("statechange", check);
  }

  container.addEventListener("controllerchange", () => {
    // The first install also takes control; only a requested update reloads.
    if (!reloading) return;
    options.reload();
  });
  container.addEventListener("message", (event) => {
    if (!isWorkerMessage(event.data)) return;
    if (event.data.type === "where") {
      event.ports[0]?.postMessage({ path: currentPath() });
      return;
    }
    for (const listener of messageListeners) listener(event.data);
  });
  container.startMessages();

  container.register(WORKER_URL, { scope: "/" }).then(
    (registration) => {
      if (registration.waiting) watch(registration.waiting);
      registration.addEventListener("updatefound", () => {
        if (registration.installing) watch(registration.installing);
      });
      options.onVisible?.(() => {
        registration.update().catch(() => {
          // Offline, or the server is restarting: the next visit looks again.
        });
      });
    },
    (error: unknown) => {
      options.warn?.(`calin: the service worker could not be registered: ${String(error)}`);
    },
  );

  return {
    onUpdateReady(listener) {
      readyListeners.add(listener);
      if (waiting) listener();
      return () => readyListeners.delete(listener);
    },
    applyUpdate() {
      if (!waiting) return;
      reloading = true;
      waiting.postMessage({ type: "activate" });
    },
    onMessage(listener) {
      messageListeners.add(listener);
      return () => messageListeners.delete(listener);
    },
  };
}
