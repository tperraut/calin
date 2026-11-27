// The service worker's entry: the real `self` handed to `createWorker`. Built on its own into
// `/sw.js` by the plugin in `vite.config.ts`, which also supplies the two constants below.
import { isPageMessage, isWhereAnswer, type WorkerMessage } from "./protocol";
import { createWorker, type WindowLike } from "./worker";

declare const self: ServiceWorkerGlobalScope;
/** A hash of the build. */
declare const __CALIN_VERSION__: string;
/** The shell's root-relative URLs (`precacheList`). */
declare const __CALIN_PRECACHE__: readonly string[];

/** How long a page has to say where it is before its creation URL is taken instead. */
const WHERE_TIMEOUT_MS = 500;

function askPath(client: WindowClient): Promise<string> {
  const fallback = new URL(client.url).pathname;
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => resolve(fallback), WHERE_TIMEOUT_MS);
    channel.port1.onmessage = (event) => {
      clearTimeout(timer);
      resolve(isWhereAnswer(event.data) ? event.data.path : fallback);
    };
    const question: WorkerMessage = { type: "where" };
    client.postMessage(question, [channel.port2]);
  });
}

function asWindow(client: WindowClient): WindowLike {
  return {
    path: () => askPath(client),
    focused: client.focused,
    visibilityState: client.visibilityState,
    focus: () => client.focus(),
    postMessage: (message: WorkerMessage) => client.postMessage(message),
  };
}

const worker = createWorker({
  version: __CALIN_VERSION__,
  precache: __CALIN_PRECACHE__,
  origin: self.location.origin,
  caches: self.caches,
  fetch: (request) => fetch(request),
  async windows() {
    const clients = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    return clients.map(asWindow);
  },
  openWindow: (url) => self.clients.openWindow(url),
  claim: () => self.clients.claim(),
  skipWaiting: () => self.skipWaiting(),
  showNotification: (title, spec) => self.registration.showNotification(title, spec),
  subscribe: (applicationServerKey) =>
    self.registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey }),
});

self.addEventListener("install", (event) => event.waitUntil(worker.install()));
self.addEventListener("activate", (event) => event.waitUntil(worker.activate()));

self.addEventListener("fetch", (event) => {
  const route = worker.route(event.request);
  if (route === "pass") return;
  event.respondWith(worker.respond(route, event.request));
});

self.addEventListener("message", (event) => {
  if (isPageMessage(event.data)) event.waitUntil(worker.activateWaiting());
});

self.addEventListener("push", (event) => {
  let text: string | undefined;
  try {
    text = event.data?.text();
  } catch {
    text = undefined;
  }
  event.waitUntil(worker.push(text));
});

self.addEventListener("notificationclick", (event) => {
  event.waitUntil(worker.notificationClick(event.notification));
});

// Not in every browser's typings yet: `newSubscription` is set where the browser renewed it itself.
type SubscriptionChangeEvent = ExtendableEvent & { newSubscription?: PushSubscription | null };

self.addEventListener("pushsubscriptionchange", (event: Event) => {
  const change = event as SubscriptionChangeEvent;
  change.waitUntil(worker.subscriptionChange(Boolean(change.newSubscription)));
});
