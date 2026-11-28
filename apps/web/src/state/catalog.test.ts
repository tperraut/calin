import { flush } from "solid-js";
import { describe, expect, it, vi } from "vite-plus/test";
import type { Api } from "../api/http";
import type { Repo } from "../gen/Repo";
import { deferred, fakeApi, makeRepo, settle } from "../test/fakes";
import { compareRepos, createCatalog } from "./catalog";

const names = (catalog: ReturnType<typeof createCatalog>) => {
  flush();
  const registry = catalog.repos();
  return registry.status === "ready" ? registry.repos.map((repo) => repo.name) : registry;
};

describe("compareRepos", () => {
  it("puts the home repo first, then orders by name", () => {
    const home = makeRepo("zeta", { home: true });
    const sorted = [makeRepo("web"), home, makeRepo("api"), makeRepo("api")].toSorted(compareRepos);
    expect(sorted.map((repo) => repo.name)).toEqual(["zeta", "api", "api", "web"]);
    expect(compareRepos(makeRepo("api"), makeRepo("api"))).toBe(0);
  });
});

describe("the repo catalog before its list was read", () => {
  it("reads the server's list after a registration instead of showing one repo", async () => {
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockResolvedValue([makeRepo("api"), makeRepo("web")]);
    const catalog = createCatalog(
      fakeApi({ listRepos, registerRepo: () => Promise.resolve(makeRepo("api")) }),
    );

    await catalog.register({ path: "/code/api" });
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(1);
    expect(names(catalog)).toEqual(["api", "web"]);
  });

  it("reads the server's list after a clone starts", async () => {
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([makeRepo("web")]);
    const catalog = createCatalog(fakeApi({ listRepos, cloneRepo: () => Promise.resolve() }));

    await catalog.clone("web");
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(1);
    expect(names(catalog)).toEqual(["web"]);
  });

  it("stays loading after a removal", async () => {
    const listRepos = vi.fn<Api["listRepos"]>();
    const catalog = createCatalog(fakeApi({ listRepos, removeRepo: () => Promise.resolve() }));

    await catalog.remove("web");
    await settle();
    expect(listRepos).not.toHaveBeenCalled();
    expect(names(catalog)).toEqual({ status: "loading" });
  });

  it("reads nothing on reload or invalidate until a screen loads it", async () => {
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([]);
    const catalog = createCatalog(fakeApi({ listRepos }));

    catalog.reload();
    catalog.invalidate();
    await settle();
    expect(listRepos).not.toHaveBeenCalled();
  });
});

describe("the repo catalog's reads", () => {
  it("asks once while a read is on its way", async () => {
    const answer = deferred<Repo[]>();
    const listRepos = vi.fn<Api["listRepos"]>().mockReturnValue(answer.promise);
    const catalog = createCatalog(fakeApi({ listRepos }));

    catalog.load();
    catalog.load();
    catalog.reload();
    expect(listRepos).toHaveBeenCalledTimes(1);
    answer.resolve([makeRepo("api")]);
    await settle();
    expect(names(catalog)).toEqual(["api"]);
  });

  it("drops an answer from before an invalidation and reads again", async () => {
    const early = deferred<Repo[]>();
    const listRepos = vi
      .fn<Api["listRepos"]>()
      .mockReturnValueOnce(early.promise)
      .mockResolvedValue([makeRepo("home", { home: true })]);
    const catalog = createCatalog(fakeApi({ listRepos }));
    catalog.load();

    catalog.invalidate();
    early.resolve([makeRepo("old")]);
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(2);
    expect(names(catalog)).toEqual(["home"]);
  });

  it("reads again on reload, or only a list that never arrived with missingOnly", async () => {
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([makeRepo("api")]);
    const catalog = createCatalog(fakeApi({ listRepos }));
    catalog.load();
    await settle();

    catalog.reload({ missingOnly: true });
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(1);
    catalog.reload();
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(2);
  });

  it("forgets what it read, and reads nothing again until loaded", async () => {
    const listRepos = vi.fn<Api["listRepos"]>().mockResolvedValue([makeRepo("api")]);
    const listRepoSets = vi
      .fn<Api["listRepoSets"]>()
      .mockResolvedValue([{ name: "all", repo_ids: ["api"] }]);
    const catalog = createCatalog(fakeApi({ listRepos, listRepoSets }));
    catalog.load();
    await settle();
    expect(catalog.sets()).toHaveLength(1);

    catalog.forget();
    flush();
    expect(names(catalog)).toEqual({ status: "loading" });
    expect(catalog.sets()).toEqual([]);
    catalog.reload();
    await settle();
    expect(listRepos).toHaveBeenCalledTimes(1);
  });
});
