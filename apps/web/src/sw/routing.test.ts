import { describe, expect, it } from "vite-plus/test";
import { routeRequest, type RequestLike } from "./routing";

const ORIGIN = "https://calin.example.com";
const PRECACHED = new Set(["/index.html", "/assets/index-abc.js", "/assets/index-def.css"]);

function request(path: string, patch: Partial<RequestLike> = {}): RequestLike {
  return {
    method: "GET",
    url: path.startsWith("http") ? path : ORIGIN + path,
    mode: "cors",
    headers: new Headers(),
    ...patch,
  };
}

const route = (one: RequestLike) => routeRequest(one, ORIGIN, PRECACHED);

describe("the service worker's routing", () => {
  it("serves page loads network first, on any app route", () => {
    for (const path of ["/", "/settings", "/s/abc", "/s/abc/changes"]) {
      expect(route(request(path, { mode: "navigate" }))).toBe("navigate");
    }
  });

  it("serves the shell of this build from its cache", () => {
    expect(route(request("/assets/index-abc.js"))).toBe("precached");
    expect(route(request("/assets/index-def.css"))).toBe("precached");
    expect(route(request("/index.html"))).toBe("precached");
  });

  it("caches other built files when they are first used", () => {
    expect(route(request("/assets/sql-123.js"))).toBe("on-use");
    expect(route(request("/assets/GraphPanel-9.css"))).toBe("on-use");
  });

  it("never handles the API, sign-in, or a socket, not even as a page load", () => {
    for (const path of [
      "/api/v1/sessions",
      "/api/v1/me",
      "/api",
      "/auth/login?next=%2F",
      "/auth/callback",
      "/auth",
    ]) {
      expect(route(request(path))).toBe("pass");
      expect(route(request(path, { mode: "navigate" }))).toBe("pass");
    }
    const upgrade = new Headers({ Upgrade: "websocket" });
    expect(route(request("/api/v1/ws", { headers: upgrade }))).toBe("pass");
    expect(route(request("/somewhere", { headers: upgrade }))).toBe("pass");
  });

  it("never handles a request that changes something", () => {
    for (const method of ["POST", "PUT", "DELETE", "PATCH", "HEAD"]) {
      expect(route(request("/assets/index-abc.js", { method }))).toBe("pass");
      expect(route(request("/", { method, mode: "navigate" }))).toBe("pass");
    }
  });

  it("leaves other origins, query strings and unknown root files to the browser", () => {
    expect(route(request("https://cdn.example.com/assets/index-abc.js"))).toBe("pass");
    expect(route(request("/assets/index-abc.js?v=2"))).toBe("pass");
    expect(route(request("/sw.js"))).toBe("pass");
    expect(route(request("/robots.txt"))).toBe("pass");
  });

  it("does not take a path that only starts like the API for it", () => {
    expect(route(request("/apiary", { mode: "navigate" }))).toBe("navigate");
    expect(route(request("/authors", { mode: "navigate" }))).toBe("navigate");
  });
});
