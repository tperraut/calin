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

type MessageFromWorker = {
  data: unknown;
  ports: ReadonlyArray<{ postMessage(answer: WhereAnswer): void }>;
};

export interface ContainerLike {
  controller: unknown;
  register(url: string, options: { scope: string }): Promise<RegistrationLike>;
  addEventListener(type: "controllerchange", listener: () => void): void;
  addEventListener(type: "message", listener: (event: MessageFromWorker) => void): void;
  startMessages(): void;
}

export interface WorkerBridge {
  /** Called once a newer build is installed and waits for the page to let it in. */
  onUpdateReady(listener: () => void): () => void;
  /** Lets the waiting build in; the page reloads once it controls it. */
  applyUpdate(): void;
  onMessage(listener: (message: WorkerMessage) => void): () => void;
}

type RegisterOptions = {
  reload: () => void;
  /** Where to look for a newer build when the page comes back to the foreground. */
  onVisible?: (check: () => void) => void;
  warn?: (message: string) => void;
  /** The path this page shows; the worker asks before it notifies (see `WhereAnswer`). */
  currentPath?: () => string;
};

class WorkerPage implements WorkerBridge {
  private readonly readyListeners = new Set<() => void>();
  private readonly messageListeners = new Set<(message: WorkerMessage) => void>();
  private readonly container: ContainerLike;
  private readonly options: RegisterOptions;
  private readonly currentPath: () => string;
  private waiting: WorkerLike | undefined;
  private reloading = false;

  constructor(container: ContainerLike, options: RegisterOptions) {
    this.container = container;
    this.options = options;
    this.currentPath = options.currentPath ?? (() => location.pathname);
  }

  start(): void {
    this.container.addEventListener("controllerchange", () => this.controllerChanged());
    this.container.addEventListener("message", (event) => this.heard(event));
    this.container.startMessages();
    this.container.register(WORKER_URL, { scope: "/" }).then(
      (registration) => this.registered(registration),
      (error: unknown) => this.unregistered(error),
    );
  }

  onUpdateReady(listener: () => void): () => void {
    this.readyListeners.add(listener);
    if (this.waiting) listener();
    return () => this.readyListeners.delete(listener);
  }

  applyUpdate(): void {
    if (!this.waiting) return;
    this.reloading = true;
    this.waiting.postMessage({ type: "activate" });
  }

  onMessage(listener: (message: WorkerMessage) => void): () => void {
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }

  private ready(worker: WorkerLike): void {
    this.waiting = worker;
    for (const listener of this.readyListeners) listener();
  }

  /** A worker that finished installing waits only if another one already runs this page. */
  private watch(worker: WorkerLike): void {
    const check = (): void => {
      if (worker.state === "installed" && this.container.controller) this.ready(worker);
    };
    check();
    worker.addEventListener("statechange", check);
  }

  private controllerChanged(): void {
    // The first install also takes control; only a requested update reloads.
    if (!this.reloading) return;
    this.options.reload();
  }

  private heard(event: MessageFromWorker): void {
    if (!isWorkerMessage(event.data)) return;
    if (event.data.type === "where") {
      event.ports[0]?.postMessage({ path: this.currentPath() });
      return;
    }
    for (const listener of this.messageListeners) listener(event.data);
  }

  private registered(registration: RegistrationLike): void {
    if (registration.waiting) this.watch(registration.waiting);
    registration.addEventListener("updatefound", () => {
      if (registration.installing) this.watch(registration.installing);
    });
    this.options.onVisible?.(() => {
      registration.update().catch(() => {
        // Offline, or the server is restarting: the next visit looks again.
      });
    });
  }

  private unregistered(error: unknown): void {
    this.options.warn?.(`calin: the service worker could not be registered: ${String(error)}`);
  }
}

/**
 * Registers the worker. A build installed while a page is open waits: it is
 * let in only by `applyUpdate`, so nothing changes under the user mid-session.
 */
export function registerWorker(container: ContainerLike, options: RegisterOptions): WorkerBridge {
  const page = new WorkerPage(container, options);
  page.start();
  return page;
}
