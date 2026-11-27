import { describe, expect, it } from "vite-plus/test";
import type { Settings } from "../gen/Settings";
import { blameSettingsField, settingsFromForm } from "./settings";

const sent: Settings = {
  home_repo: "/home/me/code/home",
  scan_roots: ["/home/me/code", "/srv/git"],
  clone_root: "/data/clones",
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

  it("blames nothing when the message points nowhere", () => {
    expect(blameSettingsField("invalid settings", sent)).toBeUndefined();
    expect(blameSettingsField("", sent)).toBeUndefined();
    // No scan root was sent, so "directory" cannot be about one.
    expect(
      blameSettingsField("not an existing directory", { ...sent, scan_roots: [] }),
    ).toBeUndefined();
  });

  it("does not guess between two fields holding the path that is quoted", () => {
    const same: Settings = { home_repo: "/code", scan_roots: ["/code"], clone_root: null };
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
    ).toEqual({ home_repo: "/code/home", scan_roots: ["/code", "/srv/git"], clone_root: null });
    expect(settingsFromForm({ homeRepo: "", scanRoots: [], cloneRoot: " /data " })).toEqual({
      home_repo: null,
      scan_roots: [],
      clone_root: "/data",
    });
  });
});
