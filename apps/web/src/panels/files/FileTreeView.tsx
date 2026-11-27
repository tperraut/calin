import { createSignal, For, Match, Show, Switch } from "solid-js";
import type { SessionRepo } from "../../gen/SessionRepo";
import { repoModeLabel } from "../../lib/status";
import type { FileTarget } from "../registry";
import { isFolder, parentKey, type FileTree, type TreeItem, type TreeRow } from "./tree";

const INDENT = 14;

const asRoot = (row: TreeRow) => (row.kind === "root" ? row : undefined);
const asEntry = (row: TreeRow) => (row.kind === "entry" ? row : undefined);
const asNote = (row: TreeRow) => (row.kind === "note" ? row : undefined);

const itemName = (row: TreeItem): string => (row.kind === "root" ? row.repo.name : row.entry.name);

/**
 * The repos as an ARIA tree, flat in the DOM with `aria-level`. One item is
 * the tab stop; arrows move, Right and Left open and close, Enter opens a
 * file, a letter jumps to the next item starting with it. The small buttons
 * at the end of a row belong to the item that has the focus.
 */
export function FileTreeView(props: {
  tree: FileTree;
  repos: readonly SessionRepo[];
  /** Key of the file showing in the editor, if any. */
  openKey: string | undefined;
  onOpen: (target: FileTarget) => void;
  onNewFile: (repo: SessionRepo, dir: string) => void;
}) {
  const [picked, setPicked] = createSignal<string>();
  let container: HTMLDivElement | undefined;

  const items = (): TreeItem[] =>
    props.tree.rows().filter((row): row is TreeItem => row.kind !== "note");
  /** The one tab stop: the item last focused, or the first while that one is out of sight. */
  const stop = (): string | undefined => {
    const all = items();
    const wanted = picked();
    return all.some((row) => row.key === wanted) ? wanted : all.at(0)?.key;
  };
  const writable = (repoId: string): SessionRepo | undefined => {
    const repo = props.repos.find((one) => one.id === repoId);
    return repo && repo.mode !== "read" ? repo : undefined;
  };

  function focusItem(key: string): void {
    setPicked(key);
    const index = items().findIndex((row) => row.key === key);
    container?.querySelectorAll<HTMLElement>('[role="treeitem"]')[index]?.focus();
  }

  function activate(row: TreeItem): void {
    setPicked(row.key);
    if (isFolder(row)) props.tree.toggle(row.key);
    else if (row.kind === "entry")
      props.onOpen({ sessionRepoId: row.repoId, path: row.entry.path });
  }

  function onKeyDown(event: KeyboardEvent): void {
    // Keys pressed on a row's buttons are theirs.
    if (!(event.target instanceof HTMLElement) || event.target.role !== "treeitem") return;
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const all = items();
    const index = all.findIndex((row) => row.key === stop());
    const row = all.at(index);
    if (index === -1 || !row) return;
    const to = (target: TreeItem | undefined): void => {
      if (target) focusItem(target.key);
    };

    if (event.key === "ArrowDown") to(all.at(index + 1));
    else if (event.key === "ArrowUp") to(index > 0 ? all.at(index - 1) : undefined);
    else if (event.key === "Home") to(all.at(0));
    else if (event.key === "End") to(all.at(-1));
    else if (event.key === "ArrowRight") {
      if (!isFolder(row)) return;
      if (row.expanded) to(all.at(index + 1));
      else props.tree.expand(row.key);
    } else if (event.key === "ArrowLeft") {
      if (isFolder(row) && row.expanded) props.tree.collapse(row.key);
      else {
        const parent = parentKey(row.key);
        to(all.find((one) => one.key === parent));
      }
    } else if (event.key === "Enter" || event.key === " ") activate(row);
    else if (event.key.length === 1) {
      // Type-ahead: the next item, wrapping around, whose name starts with the letter.
      const letter = event.key.toLowerCase();
      const after = [...all.slice(index + 1), ...all.slice(0, index + 1)];
      const match = after.find((one) => itemName(one).toLowerCase().startsWith(letter));
      if (!match) return;
      to(match);
    } else return;
    event.preventDefault();
  }

  const itemClass =
    "flex min-h-11 min-w-0 flex-1 cursor-pointer items-center gap-1.5 pr-2 text-left -outline-offset-2 hover:bg-sunken";
  const actionClass = "btn btn-ghost shrink-0 rounded-none px-0 text-muted";
  const indent = (row: TreeRow): string => `${(row.level - 1) * INDENT + 8}px`;

  return (
    <div
      ref={(element) => (container = element)}
      role="tree"
      aria-label="Files of the session's repos"
      class="min-h-0 flex-1 overflow-y-auto overscroll-contain"
      onKeyDown={onKeyDown}
    >
      <For each={props.tree.rows()} keyed={(row) => row.key}>
        {(row) => (
          <Switch>
            <Match when={asRoot(row())}>
              {(root) => (
                <div role="none" class="flex items-center border-b border-line bg-surface">
                  <div
                    role="treeitem"
                    aria-level={1}
                    aria-expanded={root().expanded ? "true" : "false"}
                    aria-selected="false"
                    tabindex={stop() === root().key ? 0 : -1}
                    class={itemClass}
                    style={{ "padding-left": indent(root()) }}
                    onClick={() => activate(root())}
                    onFocus={() => setPicked(root().key)}
                  >
                    <span aria-hidden="true" class="w-4 shrink-0 text-center text-muted">
                      {root().expanded ? "▾" : "▸"}
                    </span>
                    <span class="truncate font-semibold">{root().repo.name}</span>
                    <span class="shrink-0 rounded border border-line px-1.5 text-xs text-muted">
                      {repoModeLabel(root().repo.mode)}
                    </span>
                  </div>
                  <Show when={root().repo.mode !== "read"}>
                    <button
                      type="button"
                      class={actionClass}
                      tabindex={stop() === root().key ? 0 : -1}
                      aria-label={`New file in ${root().repo.name}`}
                      title="New file"
                      onClick={() => props.onNewFile(root().repo, "")}
                    >
                      <span aria-hidden="true">＋</span>
                    </button>
                  </Show>
                  <button
                    type="button"
                    class={actionClass}
                    tabindex={stop() === root().key ? 0 : -1}
                    aria-label={`Refresh ${root().repo.name}`}
                    title="Refresh"
                    onClick={() => props.tree.refresh(root().repo.id)}
                  >
                    <span aria-hidden="true">↻</span>
                  </button>
                </div>
              )}
            </Match>
            <Match when={asEntry(row())}>
              {(item) => (
                <div role="none" class="flex items-center">
                  <div
                    role="treeitem"
                    aria-level={item().level}
                    aria-expanded={
                      item().entry.kind === "dir" ? (item().expanded ? "true" : "false") : undefined
                    }
                    aria-selected={props.openKey === item().key ? "true" : "false"}
                    tabindex={stop() === item().key ? 0 : -1}
                    class={[
                      itemClass,
                      {
                        "text-muted": item().entry.ignored,
                        "bg-accent-soft": props.openKey === item().key,
                      },
                    ]}
                    style={{ "padding-left": indent(item()) }}
                    onClick={() => activate(item())}
                    onFocus={() => setPicked(item().key)}
                  >
                    <span aria-hidden="true" class="w-4 shrink-0 text-center text-muted">
                      {item().entry.kind === "dir" ? (item().expanded ? "▾" : "▸") : ""}
                    </span>
                    <span class="truncate font-mono text-sm">
                      {item().entry.name}
                      {item().entry.kind === "dir" ? "/" : ""}
                    </span>
                    <Show when={item().entry.kind === "symlink"}>
                      <span class="shrink-0 text-xs text-muted">link</span>
                    </Show>
                    <Show when={item().entry.ignored}>
                      <span class="shrink-0 text-xs">ignored</span>
                    </Show>
                  </div>
                  <Show when={item().entry.kind === "dir" ? writable(item().repoId) : undefined}>
                    {(repo) => (
                      <button
                        type="button"
                        class={actionClass}
                        tabindex={stop() === item().key ? 0 : -1}
                        aria-label={`New file in ${item().entry.path}`}
                        title="New file"
                        onClick={() => props.onNewFile(repo(), item().entry.path)}
                      >
                        <span aria-hidden="true">＋</span>
                      </button>
                    )}
                  </Show>
                </div>
              )}
            </Match>
            <Match when={asNote(row())}>
              {(note) => (
                <div
                  role="none"
                  class="flex min-h-9 items-center gap-2 pr-2 text-xs text-muted"
                  style={{ "padding-left": `${(note().level - 1) * INDENT + 30}px` }}
                >
                  <Switch>
                    <Match when={note().note === "loading"}>Loading…</Match>
                    <Match when={note().note === "empty"}>Empty folder</Match>
                    <Match when={note().note === "error"}>
                      <span role="alert" class="min-w-0 flex-1 text-tone-danger">
                        {note().message}
                      </span>
                      <button
                        type="button"
                        class="btn px-2 text-xs"
                        onClick={() => props.tree.reloadDir(note().repoId, note().path)}
                      >
                        Retry
                      </button>
                    </Match>
                  </Switch>
                </div>
              )}
            </Match>
          </Switch>
        )}
      </For>
    </div>
  );
}
