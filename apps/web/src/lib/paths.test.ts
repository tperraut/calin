import { describe, expect, it } from "vite-plus/test";
import { makeSessionRepo } from "../test/fakes";
import { locateInRepos } from "./paths";

const api = makeSessionRepo("api", { path: "/sessions/s1/api" });
const web = makeSessionRepo("web", { path: "/sessions/s1/web/" });
const home = makeSessionRepo("home", { path: "/home/me/notes", mode: "live" });
const inner = makeSessionRepo("inner", { path: "/home/me/notes/vendor/inner" });
const repos = [api, web, home, inner];

describe("locateInRepos", () => {
  it.each([
    ["/sessions/s1/api/src/main.rs", api, "src/main.rs"],
    ["/sessions/s1/web/index.ts", web, "index.ts"],
    ["/home/me/notes/todo.md", home, "todo.md"],
    // The repo itself.
    ["/sessions/s1/api", api, ""],
    ["/sessions/s1/api/", api, ""],
    // The innermost repo wins.
    ["/home/me/notes/vendor/inner/lib.rs", inner, "lib.rs"],
    ["/home/me/notes/vendor/other.rs", home, "vendor/other.rs"],
    ["/sessions/s1/api/a b/ü#.rs", api, "a b/ü#.rs"],
  ])("%s", (absolute, repo, path) => {
    expect(locateInRepos(absolute, repos)).toEqual({ repo, path });
  });

  it.each([
    "/sessions/s1/api-old/src/main.rs",
    "/sessions/s1",
    "/tmp/scratch.txt",
    "src/main.rs",
    "",
  ])("finds no repo for %j", (absolute) => {
    expect(locateInRepos(absolute, repos)).toBeUndefined();
  });

  it("ignores a repo without a path", () => {
    expect(locateInRepos("/x", [makeSessionRepo("ghost", { path: "" })])).toBeUndefined();
  });
});
