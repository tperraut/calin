import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ChangedFile } from "../../gen/ChangedFile";
import type { FileDiff } from "../../gen/FileDiff";
import type { RepoChanges } from "../../gen/RepoChanges";
import { deferred, makeSessionRepo, settle } from "../../test/fakes";
import {
  createRefresher,
  diffContent,
  findFile,
  inRepoOrder,
  repoTotals,
  sameDiff,
  sessionTotals,
  statusLook,
} from "./changes";

const file = (path: string, patch: Partial<ChangedFile> = {}): ChangedFile => ({
  path,
  old_path: null,
  status: "modified",
  added: 1,
  removed: 1,
  binary: false,
  ...patch,
});

const repo = (id: string, files: ChangedFile[], patch: Partial<RepoChanges> = {}): RepoChanges => ({
  session_repo_id: id,
  name: id,
  branch: null,
  base_ref: null,
  files,
  added: files.reduce((sum, one) => sum + one.added, 0),
  removed: files.reduce((sum, one) => sum + one.removed, 0),
  ...patch,
});

const diff = (patch: Partial<FileDiff> = {}): FileDiff => ({
  path: "a.txt",
  old_path: null,
  old: "one\n",
  new: "two\n",
  binary: false,
  too_large: false,
  ...patch,
});

describe("changes", () => {
  it("totals one repo from the server's own line counts", () => {
    const one = repo(
      "api",
      [file("a", { added: 3, removed: 0 }), file("logo.png", { binary: true })],
      {
        added: 40,
        removed: 7,
      },
    );
    expect(repoTotals(one)).toEqual({ files: 2, added: 40, removed: 7 });
    expect(sessionTotals([one])).toEqual({ files: 2, added: 40, removed: 7, changedRepos: 1 });
  });

  it("totals a session across repos, counting only those that changed", () => {
    const repos = [
      repo("api", [file("a", { added: 10, removed: 2 }), file("b", { added: 1, removed: 0 })]),
      repo("web", []),
      repo("docs", [file("c", { added: 0, removed: 5 })]),
      repo("infra", []),
      repo("cli", [file("d", { added: 4, removed: 4 })]),
    ];
    expect(sessionTotals(repos)).toEqual({ files: 4, added: 15, removed: 11, changedRepos: 3 });
    expect(sessionTotals([])).toEqual({ files: 0, added: 0, removed: 0, changedRepos: 0 });
  });

  it("orders repos like the session's repo set, primary first, unknown ones last", () => {
    const known = [makeSessionRepo("web", { primary: true }), makeSessionRepo("api")];
    const ordered = inRepoOrder([repo("ghost", []), repo("api", []), repo("web", [])], known);
    expect(ordered.map((one) => one.session_repo_id)).toEqual(["web", "api", "ghost"]);
  });

  it("finds a file by repo and path", () => {
    const repos = [repo("api", [file("src/a.rs")]), repo("web", [file("src/a.rs", { added: 9 })])];
    expect(findFile(repos, { sessionRepoId: "web", path: "src/a.rs" })?.added).toBe(9);
    expect(findFile(repos, { sessionRepoId: "web", path: "nope" })).toBeUndefined();
    expect(findFile(repos, { sessionRepoId: "gone", path: "src/a.rs" })).toBeUndefined();
  });

  it("gives every status a letter and a word", () => {
    expect(
      (["added", "modified", "deleted", "renamed", "untracked"] as const).map((status) => {
        const look = statusLook(status);
        return `${look.letter} ${look.label}`;
      }),
    ).toEqual(["A added", "M modified", "D deleted", "R renamed", "U untracked"]);
  });

  it("decides what a diff shows from the wire shape", () => {
    expect(diffContent(diff())).toEqual({ kind: "both", original: "one\n", modified: "two\n" });
    expect(diffContent(diff({ old: null }))).toEqual({
      kind: "single",
      side: "added",
      text: "two\n",
    });
    expect(diffContent(diff({ new: null }))).toEqual({
      kind: "single",
      side: "deleted",
      text: "one\n",
    });
    expect(diffContent(diff({ old: null, new: null, binary: true }))).toEqual({ kind: "binary" });
    // One side can still be there when the other was too large: the notice wins.
    expect(diffContent(diff({ new: null, too_large: true }))).toEqual({ kind: "too_large" });
    expect(diffContent(diff({ old: null, new: null }))).toEqual({ kind: "empty" });
    expect(diffContent(diff({ old: "", new: "" }))).toEqual({
      kind: "both",
      original: "",
      modified: "",
    });
  });

  it("tells a diff whose content changed from one that did not", () => {
    expect(sameDiff(diff(), diff())).toBe(true);
    expect(sameDiff(diff(), diff({ new: "three\n" }))).toBe(false);
    expect(sameDiff(diff(), diff({ old: null }))).toBe(false);
  });
});

describe("refresher", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces a burst of requests into one refresh", async () => {
    const run = vi.fn(() => Promise.resolve());
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);

    refresher.request();
    vi.advanceTimersByTime(300);
    refresher.request();
    vi.advanceTimersByTime(300);
    refresher.request();
    vi.advanceTimersByTime(499);
    expect(run).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(run).toHaveBeenCalledTimes(1);
    await settle();
    vi.advanceTimersByTime(5000);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("refreshes at once on demand, dropping a debounce in progress", () => {
    const run = vi.fn(() => Promise.resolve());
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);
    refresher.request();
    refresher.now();
    expect(run).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1000);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("fetches nothing while hidden, and once on the way back if anything was missed", () => {
    const run = vi.fn(() => Promise.resolve());
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);
    refresher.setVisible(false);

    refresher.request();
    refresher.request();
    refresher.now();
    vi.advanceTimersByTime(5000);
    expect(run).not.toHaveBeenCalled();

    refresher.setVisible(true);
    expect(run).toHaveBeenCalledTimes(1);
    // Nothing was missed this time.
    refresher.setVisible(false);
    refresher.setVisible(true);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("keeps a debounce that was pending when the screen hid, for the way back", () => {
    const run = vi.fn(() => Promise.resolve());
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);
    refresher.request();
    vi.advanceTimersByTime(200);
    refresher.setVisible(false);
    vi.advanceTimersByTime(1000);
    expect(run).not.toHaveBeenCalled();
    refresher.setVisible(true);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("never runs two refreshes at once: one asked meanwhile follows the first", async () => {
    const first = deferred();
    const run = vi.fn<() => Promise<void>>().mockReturnValueOnce(first.promise);
    run.mockResolvedValue(undefined);
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);
    refresher.now();
    refresher.now();
    refresher.now();
    expect(run).toHaveBeenCalledTimes(1);
    first.resolve();
    await settle();
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("goes quiet once disposed", () => {
    const run = vi.fn(() => Promise.resolve());
    const refresher = createRefresher(run, 500);
    refresher.setVisible(true);
    refresher.request();
    refresher.dispose();
    vi.advanceTimersByTime(1000);
    refresher.now();
    expect(run).not.toHaveBeenCalled();
  });
});
