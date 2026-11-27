import { createMemo, createSignal, type Accessor } from "solid-js";
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

/** The repos of a session as one tree, a root per repo, listed one folder at a time and cached. */
export function createFileTree(deps: {
  list(repoId: string, path: string): Promise<TreeEntry[]>;
  repos: Accessor<readonly SessionRepo[]>;
}): FileTree {
  // The signals follow these; see buffers.ts for why decisions are not taken on signals.
  let cache = new Map<string, DirState>();
  let open = new Set<string>();
  const [dirs, setDirs] = createSignal<ReadonlyMap<string, DirState>>(cache);
  const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(open);
  // Bumped per folder, so a listing asked before a refresh never lands after it.
  const asked = new Map<string, number>();

  function put(key: string, state: DirState): void {
    cache = new Map(cache).set(key, state);
    setDirs(cache);
  }

  function load(repoId: string, path: string): void {
    const key = dirKey(repoId, path);
    const request = (asked.get(key) ?? 0) + 1;
    asked.set(key, request);
    // A folder already listed keeps its entries on screen until the new listing lands.
    if (cache.get(key)?.status !== "ready") put(key, { status: "loading" });
    deps.list(repoId, path).then(
      (entries) => {
        if (asked.get(key) === request) put(key, { status: "ready", entries: sorted(entries) });
      },
      (error: unknown) => {
        if (asked.get(key) === request) put(key, { status: "error", message: errorMessage(error) });
      },
    );
  }

  function setOpen(next: Set<string>): void {
    open = next;
    setExpanded(open);
  }

  function expand(key: string): void {
    if (open.has(key)) return;
    setOpen(new Set(open).add(key));
    const state = cache.get(key);
    // Cached until a refresh; a failed listing is asked again.
    if (!state || state.status === "error") {
      const cut = key.indexOf("\n");
      load(key.slice(0, cut), key.slice(cut + 1));
    }
  }

  function collapse(key: string): void {
    if (!open.has(key)) return;
    const next = new Set(open);
    next.delete(key);
    setOpen(next);
  }

  const rows = createMemo((): readonly TreeRow[] => {
    const result: TreeRow[] = [];
    const listed = dirs();
    const opened = expanded();

    function children(repoId: string, path: string, level: number): void {
      const key = dirKey(repoId, path);
      const state = listed.get(key);
      const note = (kind: "loading" | "empty" | "error", message?: string): void => {
        result.push({
          kind: "note",
          key: `${key}\n:${kind}`,
          level,
          repoId,
          path,
          note: kind,
          message,
        });
      };
      if (!state || state.status === "loading") return note("loading");
      if (state.status === "error") return note("error", state.message);
      if (state.entries.length === 0) return note("empty");
      for (const entry of state.entries) {
        const entryKey = dirKey(repoId, entry.path);
        const isOpen = entry.kind === "dir" && opened.has(entryKey);
        result.push({ kind: "entry", key: entryKey, level, repoId, entry, expanded: isOpen });
        if (isOpen) children(repoId, entry.path, level + 1);
      }
    }

    for (const repo of deps.repos()) {
      const key = dirKey(repo.id, "");
      const isOpen = opened.has(key);
      result.push({ kind: "root", key, level: 1, repo, expanded: isOpen });
      if (isOpen) children(repo.id, "", 2);
    }
    return result;
  });

  return {
    rows,
    toggle(key) {
      if (open.has(key)) collapse(key);
      else expand(key);
    },
    expand,
    collapse,
    refresh(repoId) {
      const prefix = `${repoId}\n`;
      const next = new Map<string, DirState>();
      for (const [key, state] of cache) if (!key.startsWith(prefix)) next.set(key, state);
      cache = next;
      setDirs(cache);
      for (const key of open) {
        if (key.startsWith(prefix)) load(repoId, key.slice(prefix.length));
      }
    },
    reloadDir(repoId, path) {
      if (cache.has(dirKey(repoId, path))) load(repoId, path);
    },
  };
}

/** The key of the folder holding `key`'s entry; a top-level entry's parent is its root. */
export function parentKey(key: string): string | undefined {
  const cut = key.indexOf("\n");
  const path = key.slice(cut + 1);
  if (path === "") return undefined;
  const slash = path.lastIndexOf("/");
  return `${key.slice(0, cut)}\n${slash === -1 ? "" : path.slice(0, slash)}`;
}
