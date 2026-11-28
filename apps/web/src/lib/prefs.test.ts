import { describe, expect, it } from "vite-plus/test";
import {
  loadComposerPrefs,
  loadDiffLayout,
  loadEditorWrap,
  loadSidePaneWidth,
  saveComposerPrefs,
  saveDiffLayout,
  saveEditorWrap,
  saveSidePaneWidth,
} from "./prefs";

const store = (key: string, value: unknown): void =>
  localStorage.setItem(key, typeof value === "string" ? value : JSON.stringify(value));

describe("loadComposerPrefs", () => {
  it("starts from asking for every permission when nothing is stored", () => {
    expect(loadComposerPrefs()).toEqual({ permissionMode: "ask" });
  });

  it("gives back what was saved", () => {
    const prefs = {
      repos: [
        { repoId: "api", mode: "read" as const },
        { repoId: "web", mode: "live" as const },
      ],
      permissionMode: "accept_edits" as const,
      runner: "claude",
      workflow: "review",
    };
    saveComposerPrefs(prefs);
    expect(loadComposerPrefs()).toEqual(prefs);
  });

  it("keeps each repo once, drops what is not a repo, and attaches an unknown mode as a worktree", () => {
    store("calin.composer.v2", {
      repos: [
        { repoId: "api", mode: "read" },
        "web",
        null,
        { mode: "live" },
        { repoId: 7 },
        { repoId: "api", mode: "live" },
        { repoId: "docs", mode: "yolo" },
      ],
      permissionMode: "ask",
    });
    expect(loadComposerPrefs().repos).toEqual([
      { repoId: "api", mode: "read" },
      { repoId: "docs", mode: "write" },
    ]);
  });

  it("refuses each field that makes no sense and keeps the others", () => {
    store("calin.composer.v2", { permissionMode: "yolo", runner: "", workflow: 3 });
    expect(loadComposerPrefs()).toEqual({ permissionMode: "ask" });
    store("calin.composer.v2", {
      repos: [],
      permissionMode: "autonomous",
      runner: 5,
      workflow: "",
    });
    expect(loadComposerPrefs()).toEqual({ repos: [], permissionMode: "autonomous" });
  });

  it("reads the first version's mode, runner and workflow, but never its repos", () => {
    store("calin.composer.v1", {
      repos: [{ repoId: "api", mode: "write" }],
      permissionMode: "accept_edits",
      runner: "claude",
      workflow: "review",
    });
    expect(loadComposerPrefs()).toEqual({
      permissionMode: "accept_edits",
      runner: "claude",
      workflow: "review",
    });
  });

  it("prefers the current version, and falls back to the first when the current is not a record", () => {
    store("calin.composer.v1", { permissionMode: "autonomous" });
    store("calin.composer.v2", { permissionMode: "accept_edits" });
    expect(loadComposerPrefs().permissionMode).toBe("accept_edits");
    store("calin.composer.v2", '"just text"');
    expect(loadComposerPrefs()).toEqual({ permissionMode: "autonomous" });
    store("calin.composer.v2", "{not json");
    expect(loadComposerPrefs()).toEqual({ permissionMode: "autonomous" });
  });

  it("starts from the defaults when nothing stored makes sense", () => {
    store("calin.composer.v2", "[1, 2]");
    store("calin.composer.v1", "42");
    // An array is an object: it is read as a record with no field that makes sense.
    expect(loadComposerPrefs()).toEqual({ permissionMode: "ask" });
  });
});

describe("the side pane's width", () => {
  it("is the reader's own once they resized it, to the pixel", () => {
    expect(loadSidePaneWidth()).toBeUndefined();
    saveSidePaneWidth(321.6);
    expect(loadSidePaneWidth()).toBe(322);
    store("calin.sidePane.v1", 1);
    expect(loadSidePaneWidth()).toBe(1);
  });

  it("is the default for a width that cannot be one", () => {
    for (const stored of ["0", "-40", "1e999", '"320"', "null"]) {
      store("calin.sidePane.v1", stored);
      expect(loadSidePaneWidth()).toBeUndefined();
    }
  });
});

describe("the diff layout and the editor's wrapping", () => {
  it("are the reader's own choice once made, and nothing before", () => {
    expect(loadDiffLayout()).toBeUndefined();
    saveDiffLayout("split");
    expect(loadDiffLayout()).toBe("split");
    saveDiffLayout("unified");
    expect(loadDiffLayout()).toBe("unified");
    expect(loadEditorWrap()).toBeUndefined();
    saveEditorWrap(false);
    expect(loadEditorWrap()).toBe(false);
    saveEditorWrap(true);
    expect(loadEditorWrap()).toBe(true);
  });

  it("are no choice when what is stored is none of them", () => {
    store("calin.diffLayout.v1", '"side-by-side"');
    expect(loadDiffLayout()).toBeUndefined();
    store("calin.editorWrap.v1", '"yes"');
    expect(loadEditorWrap()).toBeUndefined();
  });
});
