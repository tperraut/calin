import { describe, expect, it } from "vite-plus/test";
import type { Settings } from "../gen/Settings";
import { blameSettingsField, settingsFromForm } from "./settings";

const sent: Settings = {
  home_repo: "/home/me/code/home",
  scan_roots: ["/home/me/code", "/srv/git"],
  clone_root: "/data/clones",
  max_parallel: null,
};

describe("blameSettingsField", () => {
  it.each([
    // A field named in words.
    ["home_repo: not the root of a git work tree", { field: "home_repo" }],
    ["the home repo must be a git repository", { field: "home_repo" }],
    ["scan root /srv/git does not exist", { field: "scan_roots", scanRoot: "/srv/git" }],
    ["scan_roots[0] is not a directory", { field: "scan_roots", scanRoot: undefined }],
    ["clone root must be an absolute path", { field: "clone_root" }],
    // The field named first is the subject.
    ["scan root /srv/git is inside the home repo", { field: "scan_roots", scanRoot: "/srv/git" }],
    // A path quoted back: the longest one that was sent.
    ["/home/me/code/home is not the root of a git work tree", { field: "home_repo" }],
    ["/home/me/code is not a directory", { field: "scan_roots", scanRoot: "/home/me/code" }],
    ["/data/clones cannot be created", { field: "clone_root" }],
    // Only what the rule is about.
    ["not the root of a git work tree", { field: "home_repo" }],
    ["not an existing directory", { field: "scan_roots" }],
  ])("%s", (message, blame) => {
    expect(blameSettingsField(message, sent)).toEqual(blame);
  });

  it("blames the longest path quoted, wherever it was sent", () => {
    const roots = (scan_roots: string[]): Settings => ({ ...sent, scan_roots });
    const message = "scan root /srv/git/web is inside /srv/git";
    for (const order of [
      ["/srv/git", "/srv/git/web"],
      ["/srv/git/web", "/srv/git"],
    ]) {
      expect(blameSettingsField(message, roots(order))).toEqual({
        field: "scan_roots",
        scanRoot: "/srv/git/web",
      });
    }
    const nested = (home_repo: string, scan_roots: string[]): Settings => ({
      ...sent,
      home_repo,
      scan_roots,
    });
    expect(
      blameSettingsField("/code/web: permission denied", nested("/code", ["/code/web"])),
    ).toEqual({ field: "scan_roots", scanRoot: "/code/web" });
    expect(
      blameSettingsField("/code/home: permission denied", nested("/code/home", ["/code"])),
    ).toEqual({ field: "home_repo" });
  });

  it("blames the one scan root sent for a folder that is not one", () => {
    expect(
      blameSettingsField("not an existing directory", { ...sent, scan_roots: ["/x"] }),
    ).toEqual({ field: "scan_roots" });
  });

  it("blames nothing when the message points nowhere", () => {
    expect(blameSettingsField("invalid settings", sent)).toBeUndefined();
    expect(blameSettingsField("", sent)).toBeUndefined();
    // No scan root was sent, so "directory" cannot be about one.
    expect(
      blameSettingsField("not an existing directory", { ...sent, scan_roots: [] }),
    ).toBeUndefined();
  });

  it("does not guess between two fields holding the path that is quoted", () => {
    const same: Settings = {
      home_repo: "/code",
      scan_roots: ["/code"],
      clone_root: null,
      max_parallel: null,
    };
    expect(blameSettingsField("/code: permission denied", same)).toBeUndefined();
    // The rule still tells.
    expect(blameSettingsField("/code is not a git work tree", same)).toEqual({
      field: "home_repo",
    });
  });
});

describe("settingsFromForm", () => {
  it("trims, drops blank and repeated roots, and sends an empty path as not set", () => {
    expect(
      settingsFromForm({
        homeRepo: "  /code/home ",
        scanRoots: ["/code", " /code ", "", "  ", "/srv/git"],
        cloneRoot: "   ",
      }),
    ).toEqual({
      home_repo: "/code/home",
      scan_roots: ["/code", "/srv/git"],
      clone_root: null,
      max_parallel: null,
    });
    expect(settingsFromForm({ homeRepo: "", scanRoots: [], cloneRoot: " /data " })).toEqual({
      home_repo: null,
      scan_roots: [],
      clone_root: "/data",
      max_parallel: null,
    });
  });

  it("sends the number of sessions running at once as a number, and empty as not set", () => {
    const form = { homeRepo: "", scanRoots: [], cloneRoot: "" };
    expect(settingsFromForm({ ...form, maxParallel: " 6 " }).max_parallel).toBe(6);
    expect(settingsFromForm({ ...form, maxParallel: "" }).max_parallel).toBeNull();
    // Not ours to judge: the server refuses what it does not accept, in its own words.
    expect(settingsFromForm({ ...form, maxParallel: "0" }).max_parallel).toBe(0);
  });

  it("blames the parallel limit for a 400 that names it", () => {
    const sent: Settings = { home_repo: null, scan_roots: [], clone_root: null, max_parallel: 0 };
    expect(blameSettingsField("max_parallel must be between 1 and 64", sent)).toEqual({
      field: "max_parallel",
    });
  });
});
