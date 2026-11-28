import { createMemo, createSignal, type Accessor, type Setter } from "solid-js";
import type { SessionRepo } from "../../gen/SessionRepo";
import type { TreeEntry } from "../../gen/TreeEntry";
import { errorMessage } from "../../lib/errors";

type DirState =
  | { status: "loading" }
  | { status: "ready"; entries: readonly TreeEntry[] }
  | { status: "error"; message: string };

/**
 * One line of the tree as it shows, top to bottom. `root` and `entry` rows
 * are the tree's items; `note` rows say what an open folder has instead of
 * entries (still loading, empty, failed) and are not items.
 */
export type TreeRow =
  | { kind: "root"; key: string; level: 1; repo: SessionRepo; expanded: boolean }
  | {
      kind: "entry";
      key: string;
      level: number;
      repoId: string;
      entry: TreeEntry;
      /** Folders only. */
      expanded: boolean;
    }
  | {
      kind: "note";
      key: string;
      level: number;
      repoId: string;
      path: string;
      note: "loading" | "empty" | "error";
      message?: string;
    };

export type TreeItem = Exclude<TreeRow, { kind: "note" }>;

export type FileTree = {
  rows: Accessor<readonly TreeRow[]>;
  /** Opens or closes a folder or a root; a folder is listed the first time it opens. */
  toggle(key: string): void;
  expand(key: string): void;
  collapse(key: string): void;
  /** Forgets what was listed under this root and lists its open folders again. */
  refresh(repoId: string): void;
  /** Lists one folder again, when something was just created in it. */
  reloadDir(repoId: string, path: string): void;
};

/** A root's key is its folder's: the empty path. */
export const dirKey = (repoId: string, path: string): string => `${repoId}\n${path}`;

export const isFolder = (row: TreeItem): boolean => row.kind === "root" || row.entry.kind === "dir";

/** Folders first, then by name, whatever order the server listed them in. */
function sorted(entries: readonly TreeEntry[]): TreeEntry[] {
  return entries.toSorted((a, b) => {
    const folders = Number(b.kind === "dir") - Number(a.kind === "dir");
    return folders !== 0 ? folders : a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
  });
}

type NoteKind = "loading" | "empty" | "error";

const noteRow = (
  repoId: string,
  path: string,
  level: number,
  note: NoteKind,
  message?: string,
): TreeRow => ({
  kind: "note",
  key: `${dirKey(repoId, path)}\n:${note}`,
  level,
  repoId,
  path,
  note,
  message,
});

/** What an open folder with no entries to show says instead: still loading, failed, or empty. */
function folderNote(state: DirState | undefined, repoId: string, path: string, level: number) {
  if (!state || state.status === "loading") return noteRow(repoId, path, level, "loading");
  if (state.status === "error") return noteRow(repoId, path, level, "error", state.message);
  return noteRow(repoId, path, level, "empty");
}

/** What the rows are made from: the listed folders, and which folders are open. */
type TreeState = { listed: ReadonlyMap<string, DirState>; opened: ReadonlySet<string> };

/** Appends the rows under open folder `path` to `rows`, its open subfolders' included. */
function folderRows(
  rows: TreeRow[],
  tree: TreeState,
  repoId: string,
  path: string,
  level: number,
): void {
  const state = tree.listed.get(dirKey(repoId, path));
  if (state?.status !== "ready" || state.entries.length === 0) {
    rows.push(folderNote(state, repoId, path, level));
    return;
  }
  for (const entry of state.entries) {
    const key = dirKey(repoId, entry.path);
    const isOpen = entry.kind === "dir" && tree.opened.has(key);
    rows.push({ kind: "entry", key, level, repoId, entry, expanded: isOpen });
    if (isOpen) folderRows(rows, tree, repoId, entry.path, level + 1);
  }
}

/** The tree as it shows, top to bottom: a root per repo, and what is listed under the open ones. Pure. */
export function treeRows(repos: readonly SessionRepo[], tree: TreeState): TreeRow[] {
  const rows: TreeRow[] = [];
  for (const repo of repos) {
    const key = dirKey(repo.id, "");
    const isOpen = tree.opened.has(key);
    rows.push({ kind: "root", key, level: 1, repo, expanded: isOpen });
    if (isOpen) folderRows(rows, tree, repo.id, "", 2);
  }
  return rows;
}

/** The repos of a session as one tree, a root per repo, listed one folder at a time and cached. */
export function createFileTree(deps: TreeDeps): FileTree {
  return new RepoTree(deps);
}

type TreeDeps = {
  list(repoId: string, path: string): Promise<TreeEntry[]>;
  repos: Accessor<readonly SessionRepo[]>;
};

// A class rather than a closure of methods: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class RepoTree implements FileTree {
  readonly rows: Accessor<readonly TreeRow[]>;
  private readonly deps: TreeDeps;
  // The signals follow these; see buffers.ts for why decisions are not taken on signals.
  private cache = new Map<string, DirState>();
  private open = new Set<string>();
  private readonly setDirs: Setter<ReadonlyMap<string, DirState>>;
  private readonly setExpanded: Setter<ReadonlySet<string>>;
  // Bumped per folder, so a listing asked before a refresh never lands after it.
  private readonly asked = new Map<string, number>();

  constructor(deps: TreeDeps) {
    this.deps = deps;
    const [dirs, setDirs] = createSignal<ReadonlyMap<string, DirState>>(this.cache);
    const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(this.open);
    this.setDirs = setDirs;
    this.setExpanded = setExpanded;
    this.rows = createMemo(() => treeRows(deps.repos(), { listed: dirs(), opened: expanded() }));
  }

  toggle(key: string): void {
    if (this.open.has(key)) this.collapse(key);
    else this.expand(key);
  }

  expand(key: string): void {
    if (this.open.has(key)) return;
    this.setOpen(new Set(this.open).add(key));
    const state = this.cache.get(key);
    // Cached until a refresh; a failed listing is asked again.
    if (!state || state.status === "error") {
      const cut = key.indexOf("\n");
      this.load(key.slice(0, cut), key.slice(cut + 1));
    }
  }

  collapse(key: string): void {
    if (!this.open.has(key)) return;
    const next = new Set(this.open);
    next.delete(key);
    this.setOpen(next);
  }

  refresh(repoId: string): void {
    const prefix = `${repoId}\n`;
    this.setCache(new Map([...this.cache].filter(([key]) => !key.startsWith(prefix))));
    for (const key of this.open) {
      if (key.startsWith(prefix)) this.load(repoId, key.slice(prefix.length));
    }
  }

  reloadDir(repoId: string, path: string): void {
    if (this.cache.has(dirKey(repoId, path))) this.load(repoId, path);
  }

  private setCache(next: Map<string, DirState>): void {
    this.cache = next;
    this.setDirs(next);
  }

  private setOpen(next: Set<string>): void {
    this.open = next;
    this.setExpanded(next);
  }

  private put(key: string, state: DirState): void {
    this.setCache(new Map(this.cache).set(key, state));
  }

  private load(repoId: string, path: string): void {
    const key = dirKey(repoId, path);
    const request = (this.asked.get(key) ?? 0) + 1;
    this.asked.set(key, request);
    // A folder already listed keeps its entries on screen until the new listing lands.
    if (this.cache.get(key)?.status !== "ready") this.put(key, { status: "loading" });
    const settle = (state: DirState): void => {
      if (this.asked.get(key) === request) this.put(key, state);
    };
    this.deps.list(repoId, path).then(
      (entries) => settle({ status: "ready", entries: sorted(entries) }),
      (error: unknown) => settle({ status: "error", message: errorMessage(error) }),
    );
  }
}
/** The key of the folder holding `key`'s entry; a top-level entry's parent is its root. */
export function parentKey(key: string): string | undefined {
  const cut = key.indexOf("\n");
  const path = key.slice(cut + 1);
  if (path === "") return undefined;
  const slash = path.lastIndexOf("/");
  return `${key.slice(0, cut)}\n${slash === -1 ? "" : path.slice(0, slash)}`;
}
