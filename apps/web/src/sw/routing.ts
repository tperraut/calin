/**
 * How the service worker treats a request:
 * - `navigate`: a page load. Network first, the cached shell when the network fails.
 * - `precached`: part of the shell of this build. From the cache.
 * - `on-use`: a fingerprinted file the shell loads later (a panel, a grammar). Cached the
 *   first time it is fetched; its name changes whenever its content does.
 * - `pass`: not the worker's business. The browser fetches it as if there were no worker,
 *   and nothing of it is cached. Always the answer for the API, sign-in and sockets.
 */
export type Route = "navigate" | "precached" | "on-use" | "pass";

/** The parts of a `Request` the decision reads. */
export type RequestLike = {
  method: string;
  url: string;
  mode: string;
  headers: { get(name: string): string | null };
};

/** Paths whose answers depend on who asks, or that change state: never served by the worker. */
const NEVER = ["/api", "/auth"];

function underNever(pathname: string): boolean {
  return NEVER.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

/** False for what is not this app's own to serve: another origin, the API, sign-in, sockets. */
function servedHere(request: RequestLike, url: URL, origin: string): boolean {
  return (
    url.origin === origin &&
    !underNever(url.pathname) &&
    request.headers.get("upgrade")?.toLowerCase() !== "websocket"
  );
}

/** A file request: the shell, a fingerprinted file, or something else. */
function fileRoute(url: URL, precached: ReadonlySet<string>): Route {
  // A query string asks for something else than the file the build made.
  if (url.search !== "") return "pass";
  if (precached.has(url.pathname)) return "precached";
  if (url.pathname.startsWith("/assets/")) return "on-use";
  return "pass";
}

export function routeRequest(
  request: RequestLike,
  origin: string,
  precached: ReadonlySet<string>,
): Route {
  if (request.method !== "GET") return "pass";
  const url = new URL(request.url);
  if (!servedHere(request, url, origin)) return "pass";
  if (request.mode === "navigate") return "navigate";
  return fileRoute(url, precached);
}
