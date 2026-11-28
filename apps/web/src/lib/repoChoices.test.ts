import { describe, expect, it } from "vite-plus/test";
import type { Repo } from "../gen/Repo";
import { makeMissingRepo, makeRepo } from "../test/fakes";
import { filteredRepos, leftOutNote, selectionSummary, setChosen, toggledSet } from "./repoChoices";

const byId = (repos: Repo[]): ReadonlyMap<string, Repo> =>
  new Map(repos.map((repo) => [repo.id, repo]));

describe("the repo picker's choices", () => {
  it("counts a set with one cloned repo as chosen once that repo is, and unchooses it", () => {
    const catalog = byId([makeRepo("api"), makeMissingRepo("docs")]);
    const set = { name: "backend", repo_ids: ["api", "docs"] };
    const chosen = [{ repoId: "api", mode: "write" as const }];
    expect(setChosen(chosen, set, catalog)).toBe(true);
    expect(toggledSet(chosen, set, catalog)).toEqual([]);
    expect(setChosen([], set, catalog)).toBe(false);
  });

  it("names every repo of a set left out because it is not cloned", () => {
    const missing = ["a", "b", "c"].map((id) => makeMissingRepo(id));
    const set = { name: "all", repo_ids: ["a", "b", "c"] };
    expect(leftOutNote(set, byId(missing))).toBe(
      "all: a, b and c are not cloned and were left out.",
    );
  });

  it("counts the repos after the primary one in words", () => {
    expect(selectionSummary([])).toBe("none selected");
    expect(selectionSummary(["api"])).toBe("api");
    expect(selectionSummary(["api", "web"])).toBe("api + 1 repo");
    expect(selectionSummary(["api", "web", "docs"])).toBe("api + 2 repos");
  });

  it("filters by name only a list long enough, and only with a filter", () => {
    const repos = ["api", "web", "docs"].map((id) => makeRepo(id));
    expect(filteredRepos(repos, "WE", 3).map((repo) => repo.id)).toEqual(["web"]);
    expect(filteredRepos(repos, "we", 4)).toBe(repos);
    expect(filteredRepos(repos, "  ", 3)).toBe(repos);
  });
});
