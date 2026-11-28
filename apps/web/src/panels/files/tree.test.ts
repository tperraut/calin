import { createRoot, createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { SessionRepo } from "../../gen/SessionRepo";
import type { TreeEntry } from "../../gen/TreeEntry";
import { deferred, makeSessionRepo, settle } from "../../test/fakes";
import {
  createFileTree,
  dirKey,
  isFolder,
  parentKey,
  treeRows,
  type FileTree,
  type TreeRow,
} from "./tree";

const entry = (path: string, kind: TreeEntry["kind"] = "file", ignored = false): TreeEntry => ({
  name: path.slice(path.lastIndexOf("/") + 1),
  path,
  kind,
  size: kind === "file" ? 10 : null,
  ignored,
});

const listings: Record<string, TreeEntry[]> = {
  "api\n": [entry("README.md"), entry("src", "dir"), entry("Cargo.toml"), entry("assets", "dir")],
  "api\nsrc": [entry("src/main.rs"), entry("src/api", "dir")],
  "api\nsrc/api": [],
  "web\n": [entry("package.json")],
};

let dispose = (): void => {};
afterEach(() => dispose());

function setup(
  repos: SessionRepo[] = [makeSessionRepo("api", { primary: true }), makeSessionRepo("web")],
) {
  const list = vi.fn((repoId: string, path: string) =>
    Promise.resolve(listings[dirKey(repoId, path)] ?? []),
  );
  const [known, setKnown] = createSignal<readonly SessionRepo[]>(repos);
  let tree!: FileTree;
  dispose = createRoot((stop) => {
    tree = createFileTree({ list, repos: known });
    return stop;
  });
  const rows = (): string[] => {
    flush();
    return tree.rows().map(describe_);
  };
  return { tree, list, rows, setKnown };
}

const describe_ = (row: TreeRow): string => {
  const pad = "  ".repeat(row.level - 1);
  if (row.kind === "root") return `${pad}${row.expanded ? "v" : ">"} ${row.repo.name}`;
  if (row.kind === "note") return `${pad}(${row.note}${row.message ? `: ${row.message}` : ""})`;
  const mark = row.entry.kind === "dir" ? (row.expanded ? "v " : "> ") : "";
  return `${pad}${mark}${row.entry.name}`;
};

describe("file tree", () => {
  it("starts as one closed root per repo, in the session's order, and lists nothing", () => {
    const { rows, list } = setup();
    expect(rows()).toEqual(["> api", "> web"]);
    expect(list).not.toHaveBeenCalled();
  });

  it("lists one level when a root opens, folders first, then by name", async () => {
    const { tree, rows, list } = setup();
    tree.expand(dirKey("api", ""));
    expect(rows()).toEqual(["v api", "  (loading)", "> web"]);
    await settle();
    expect(rows()).toEqual([
      "v api",
      "  > assets",
      "  > src",
      "  Cargo.toml",
      "  README.md",
      "> web",
    ]);
    expect(list.mock.calls).toEqual([["api", ""]]);
  });

  it("loads a folder the first time it opens, and keeps it after that", async () => {
    const { tree, rows, list } = setup();
    tree.expand(dirKey("api", ""));
    await settle();
    tree.toggle(dirKey("api", "src"));
    await settle();
    expect(rows()).toEqual([
      "v api",
      "  > assets",
      "  v src",
      "    > api",
      "    main.rs",
      "  Cargo.toml",
      "  README.md",
      "> web",
    ]);
    tree.toggle(dirKey("api", "src/api"));
    await settle();
    expect(rows()).toContain("      (empty)");

    // Closing and opening again asks the server nothing.
    tree.toggle(dirKey("api", "src"));
    expect(rows()).not.toContain("    main.rs");
    tree.toggle(dirKey("api", "src"));
    tree.collapse(dirKey("api", ""));
    tree.expand(dirKey("api", ""));
    expect(rows()).toContain("    main.rs");
    expect(list.mock.calls).toEqual([
      ["api", ""],
      ["api", "src"],
      ["api", "src/api"],
    ]);
  });

  it("forgets one root on refresh and lists its open folders again, leaving the others alone", async () => {
    const { tree, rows, list } = setup();
    tree.expand(dirKey("api", ""));
    tree.expand(dirKey("web", ""));
    await settle();
    tree.expand(dirKey("api", "src"));
    await settle();
    list.mockClear();

    listings["api\nsrc"] = [...listings["api\nsrc"], entry("src/health.rs")];
    tree.refresh("api");
    expect(rows()).toEqual(["v api", "  (loading)", "v web", "  package.json"]);
    await settle();
    expect(rows()).toContain("    health.rs");
    expect(list.mock.calls.map(([repoId, path]) => `${repoId}:${path}`).toSorted()).toEqual([
      "api:",
      "api:src",
    ]);
    listings["api\nsrc"] = listings["api\nsrc"].slice(0, 2);
  });

  it("shows the server's message when a folder cannot be listed, and asks again on the next open", async () => {
    const { tree, rows, list } = setup();
    list.mockRejectedValueOnce(new Error("src is not a directory"));
    tree.expand(dirKey("api", ""));
    await settle();
    expect(rows()).toEqual(["v api", "  (error: src is not a directory)", "> web"]);
    tree.collapse(dirKey("api", ""));
    tree.expand(dirKey("api", ""));
    expect(rows()).toEqual(["v api", "  (loading)", "> web"]);
    await settle();
    expect(rows()).toContain("  Cargo.toml");
  });

  it("keeps a listed folder's entries on screen while it is listed again", async () => {
    const { tree, rows } = setup();
    tree.expand(dirKey("api", ""));
    await settle();
    tree.reloadDir("api", "");
    expect(rows()).toContain("  Cargo.toml");
    expect(rows()).not.toContain("  (loading)");
  });

  it("drops a listing that answers after a refresh asked for a newer one", async () => {
    const { tree, rows, list } = setup();
    const slow = deferred<TreeEntry[]>();
    list.mockReturnValueOnce(slow.promise);
    tree.expand(dirKey("api", ""));
    tree.refresh("api");
    await settle();
    slow.resolve([entry("stale.txt")]);
    await settle();
    expect(rows()).not.toContain("  stale.txt");
    expect(rows()).toContain("  Cargo.toml");
  });

  it("drops a failure that answers after a refresh asked again", async () => {
    const { tree, rows, list } = setup();
    const slow = deferred<TreeEntry[]>();
    list.mockReturnValueOnce(slow.promise);
    tree.expand(dirKey("api", ""));
    tree.refresh("api");
    await settle();
    slow.reject(new Error("stale failure"));
    await settle();
    expect(rows()).toContain("  Cargo.toml");
    expect(rows().join()).not.toContain("stale failure");
  });

  it("asks nothing again of a folder already open, closed, or never listed", async () => {
    const { tree, rows, list } = setup();
    tree.collapse(dirKey("api", ""));
    tree.expand(dirKey("api", ""));
    tree.expand(dirKey("api", ""));
    await settle();
    tree.reloadDir("api", "src");
    tree.reloadDir("web", "");
    expect(list.mock.calls).toEqual([["api", ""]]);
    // A folder listed before is listed again, as after a file was created in it.
    tree.reloadDir("api", "");
    await settle();
    expect(list.mock.calls).toEqual([
      ["api", ""],
      ["api", ""],
    ]);
    expect(rows()).toContain("  Cargo.toml");
  });

  it("follows the repo set", () => {
    const { rows, setKnown } = setup();
    setKnown([makeSessionRepo("web"), makeSessionRepo("docs", { mode: "read" })]);
    expect(rows()).toEqual(["> web", "> docs"]);
  });

  it("tells folders, roots included, from files", async () => {
    const { tree } = setup();
    tree.expand(dirKey("api", ""));
    await settle();
    flush();
    const folders = tree
      .rows()
      .flatMap((row) => (row.kind === "note" ? [] : [`${row.key}:${isFolder(row)}`]));
    expect(folders).toEqual([
      "api\n:true",
      "api\nassets:true",
      "api\nsrc:true",
      "api\nCargo.toml:false",
      "api\nREADME.md:false",
      "web\n:true",
    ]);
  });

  it("says an open folder nothing was listed for yet is loading", () => {
    const repos = [makeSessionRepo("api")];
    const opened = new Set([dirKey("api", "")]);
    const rows = treeRows(repos, { listed: new Map(), opened }).map(describe_);
    expect(rows).toEqual(["v api", "  (loading)"]);
  });

  it("knows the parent of an entry", () => {
    expect(parentKey(dirKey("api", "src/api/mod.rs"))).toBe(dirKey("api", "src/api"));
    expect(parentKey(dirKey("api", "README.md"))).toBe(dirKey("api", ""));
    expect(parentKey(dirKey("api", ""))).toBeUndefined();
    // A newline is legal in a file name; only the first one separates the repo.
    expect(parentKey(dirKey("api", "a\nb/c"))).toBe(dirKey("api", "a\nb"));
  });
});
