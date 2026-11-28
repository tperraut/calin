import { createSignal, For, Match, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { SessionRepo } from "../../gen/SessionRepo";
import { repoModeLabel } from "../../lib/status";
import type { FileTarget } from "../registry";
import { isFolder, parentKey, type FileTree, type TreeItem, type TreeRow } from "./tree";

const INDENT = 14;

type RootItem = Extract<TreeRow, { kind: "root" }>;
type EntryItem = Extract<TreeRow, { kind: "entry" }>;
type NoteItem = Extract<TreeRow, { kind: "note" }>;

const asRoot = (row: TreeRow) => (row.kind === "root" ? row : undefined);
const asEntry = (row: TreeRow) => (row.kind === "entry" ? row : undefined);
const asNote = (row: TreeRow) => (row.kind === "note" ? row : undefined);
const isItem = (row: TreeRow): row is TreeItem => row.kind !== "note";

const itemName = (row: TreeItem): string => (row.kind === "root" ? row.repo.name : row.entry.name);

/**
 * What a key does to the tree. `focus` without an item is a key of the
 * tree's that has nowhere to go: it is still the tree's.
 */
type TreeKeyAction =
  | { do: "focus"; to: TreeItem | undefined }
  | { do: "expand" | "collapse"; key: string }
  | { do: "activate" };

type Move = (all: readonly TreeItem[], index: number) => TreeKeyAction | undefined;

const focus = (to: TreeItem | undefined): TreeKeyAction => ({ do: "focus", to });

/** Right: opens a closed folder, steps into an open one; a file's is not the tree's. */
const inward: Move = (all, index) => {
  const row = all[index];
  if (!isFolder(row)) return undefined;
  return row.expanded ? focus(all.at(index + 1)) : { do: "expand", key: row.key };
};

/** Left: closes an open folder, else goes to the folder holding the item. */
const outward: Move = (all, index) => {
  const row = all[index];
  if (isFolder(row) && row.expanded) return { do: "collapse", key: row.key };
  const parent = parentKey(row.key);
  return focus(all.find((one) => one.key === parent));
};

const MOVES = new Map<string, Move>([
  ["ArrowDown", (all, index) => focus(all.at(index + 1))],
  ["ArrowUp", (all, index) => focus(index > 0 ? all.at(index - 1) : undefined)],
  ["Home", (all) => focus(all.at(0))],
  ["End", (all) => focus(all.at(-1))],
  ["ArrowRight", inward],
  ["ArrowLeft", outward],
  ["Enter", () => ({ do: "activate" })],
  [" ", () => ({ do: "activate" })],
]);

/** Type-ahead: the next item, wrapping around, whose name starts with the letter. */
function typeAhead(letter: string, all: readonly TreeItem[], index: number) {
  const lower = letter.toLowerCase();
  const after = [...all.slice(index + 1), ...all.slice(0, index + 1)];
  const match = after.find((one) => itemName(one).toLowerCase().startsWith(lower));
  return match ? focus(match) : undefined;
}

/** What `key` does with the focus on `all[index]`; undefined when the key is not the tree's. Pure. */
function treeKeyAction(
  key: string,
  all: readonly TreeItem[],
  index: number,
): TreeKeyAction | undefined {
  const move = MOVES.get(key);
  if (move) return move(all, index);
  return key.length === 1 ? typeAhead(key, all, index) : undefined;
}

/** A key pressed on an item with no modifier; keys pressed on a row's buttons are theirs. */
const isTreeKey = (event: KeyboardEvent): boolean =>
  event.target instanceof HTMLElement &&
  event.target.role === "treeitem" &&
  !(event.ctrlKey || event.metaKey || event.altKey);

/** Does what a key asked, with the focus on `row`. */
function runTreeKey(
  action: TreeKeyAction,
  row: TreeItem,
  on: { tree: FileTree; focus: (key: string) => void; activate: (row: TreeItem) => void },
): void {
  if (action.do === "focus") {
    if (action.to) on.focus(action.to.key);
  } else if (action.do === "expand") on.tree.expand(action.key);
  else if (action.do === "collapse") on.tree.collapse(action.key);
  else on.activate(row);
}

/** The one tab stop: the item last focused, or the first while that one is out of sight. */
function tabStop(all: readonly TreeItem[], wanted: string | undefined): string | undefined {
  return all.some((row) => row.key === wanted) ? wanted : all.at(0)?.key;
}

/** The repo to create a file in from this folder row, if it takes writes. */
function newFileRepo(repos: readonly SessionRepo[], row: EntryItem): SessionRepo | undefined {
  if (row.entry.kind !== "dir") return undefined;
  const repo = repos.find((one) => one.id === row.repoId);
  return repo && repo.mode !== "read" ? repo : undefined;
}

const itemClass =
  "flex min-h-11 min-w-0 flex-1 cursor-pointer items-center gap-1.5 pr-2 text-left -outline-offset-2 hover:bg-sunken";
const actionClass = "btn btn-ghost shrink-0 rounded-none px-0 text-muted";
const indent = (row: TreeRow): string => `${(row.level - 1) * INDENT + 8}px`;
const tabIndex = (focusable: boolean): number => (focusable ? 0 : -1);
const flag = (on: boolean): "true" | "false" => (on ? "true" : "false");
const arrow = (row: TreeItem): string => (isFolder(row) ? (row.expanded ? "▾" : "▸") : "");

/** What every item row shares: the focus and the click. */
type ItemProps = {
  focusable: boolean;
  onActivate: (row: TreeItem) => void;
  onFocus: (key: string) => void;
};

/** The tree item itself: its arrow, then `children`. */
function Item(
  props: ItemProps & {
    row: TreeItem;
    expanded: "true" | "false" | undefined;
    selected: boolean;
    classes?: Record<string, boolean>;
    children: JSX.Element;
  },
) {
  return (
    <div
      role="treeitem"
      aria-level={props.row.level}
      aria-expanded={props.expanded}
      aria-selected={flag(props.selected)}
      tabindex={tabIndex(props.focusable)}
      class={props.classes ? [itemClass, props.classes] : itemClass}
      style={{ "padding-left": indent(props.row) }}
      onClick={() => props.onActivate(props.row)}
      onFocus={() => props.onFocus(props.row.key)}
    >
      <span aria-hidden="true" class="w-4 shrink-0 text-center text-muted">
        {arrow(props.row)}
      </span>
      {props.children}
    </div>
  );
}

/** One of the small buttons at the end of a row. */
function RowAction(props: {
  focusable: boolean;
  label: string;
  title: string;
  glyph: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      class={actionClass}
      tabindex={tabIndex(props.focusable)}
      aria-label={props.label}
      title={props.title}
      onClick={() => props.onClick()}
    >
      <span aria-hidden="true">{props.glyph}</span>
    </button>
  );
}

function RootRow(
  props: ItemProps & {
    root: RootItem;
    onNewFile: (repo: SessionRepo, dir: string) => void;
    onRefresh: (repoId: string) => void;
  },
) {
  return (
    <div role="none" class="flex items-center border-b border-line bg-surface">
      <Item
        row={props.root}
        expanded={flag(props.root.expanded)}
        selected={false}
        focusable={props.focusable}
        onActivate={props.onActivate}
        onFocus={props.onFocus}
      >
        <span class="truncate font-semibold">{props.root.repo.name}</span>
        <span class="shrink-0 rounded border border-line px-1.5 text-xs text-muted">
          {repoModeLabel(props.root.repo.mode)}
        </span>
      </Item>
      <Show when={props.root.repo.mode !== "read"}>
        <RowAction
          focusable={props.focusable}
          label={`New file in ${props.root.repo.name}`}
          title="New file"
          glyph="＋"
          onClick={() => props.onNewFile(props.root.repo, "")}
        />
      </Show>
      <RowAction
        focusable={props.focusable}
        label={`Refresh ${props.root.repo.name}`}
        title="Refresh"
        glyph="↻"
        onClick={() => props.onRefresh(props.root.repo.id)}
      />
    </div>
  );
}

function EntryRow(
  props: ItemProps & {
    item: EntryItem;
    selected: boolean;
    /** Where a file can be created from this row; only a folder of a repo that takes writes has one. */
    newFileRepo: SessionRepo | undefined;
    onNewFile: (repo: SessionRepo, dir: string) => void;
  },
) {
  const isDir = () => props.item.entry.kind === "dir";
  return (
    <div role="none" class="flex items-center">
      <Item
        row={props.item}
        expanded={isDir() ? flag(props.item.expanded) : undefined}
        selected={props.selected}
        classes={{ "text-muted": props.item.entry.ignored, "bg-accent-soft": props.selected }}
        focusable={props.focusable}
        onActivate={props.onActivate}
        onFocus={props.onFocus}
      >
        <span class="truncate font-mono text-sm">
          {props.item.entry.name}
          {isDir() ? "/" : ""}
        </span>
        <Show when={props.item.entry.kind === "symlink"}>
          <span class="shrink-0 text-xs text-muted">link</span>
        </Show>
        <Show when={props.item.entry.ignored}>
          <span class="shrink-0 text-xs">ignored</span>
        </Show>
      </Item>
      <Show when={props.newFileRepo}>
        {(repo) => (
          <RowAction
            focusable={props.focusable}
            label={`New file in ${props.item.entry.path}`}
            title="New file"
            glyph="＋"
            onClick={() => props.onNewFile(repo(), props.item.entry.path)}
          />
        )}
      </Show>
    </div>
  );
}

function NoteRow(props: { note: NoteItem; onRetry: (repoId: string, path: string) => void }) {
  return (
    <div
      role="none"
      class="flex min-h-9 items-center gap-2 pr-2 text-xs text-muted"
      style={{ "padding-left": `${(props.note.level - 1) * INDENT + 30}px` }}
    >
      <Switch>
        <Match when={props.note.note === "loading"}>Loading…</Match>
        <Match when={props.note.note === "empty"}>Empty folder</Match>
        <Match when={props.note.note === "error"}>
          <span role="alert" class="min-w-0 flex-1 text-tone-danger">
            {props.note.message}
          </span>
          <button
            type="button"
            class="btn px-2 text-xs"
            onClick={() => props.onRetry(props.note.repoId, props.note.path)}
          >
            Retry
          </button>
        </Match>
      </Switch>
    </div>
  );
}

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

  const items = (): TreeItem[] => props.tree.rows().filter(isItem);
  const stop = (): string | undefined => tabStop(items(), picked());

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
    if (!isTreeKey(event)) return;
    const all = items();
    const index = all.findIndex((row) => row.key === stop());
    const action = index === -1 ? undefined : treeKeyAction(event.key, all, index);
    if (!action) return;
    runTreeKey(action, all[index], { tree: props.tree, focus: focusItem, activate });
    event.preventDefault();
  }

  const shared = {
    onActivate: activate,
    onFocus: (key: string) => setPicked(key),
  };

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
                <RootRow
                  {...shared}
                  root={root()}
                  focusable={stop() === root().key}
                  onNewFile={props.onNewFile}
                  onRefresh={(repoId) => props.tree.refresh(repoId)}
                />
              )}
            </Match>
            <Match when={asEntry(row())}>
              {(item) => (
                <EntryRow
                  {...shared}
                  item={item()}
                  focusable={stop() === item().key}
                  selected={props.openKey === item().key}
                  newFileRepo={newFileRepo(props.repos, item())}
                  onNewFile={props.onNewFile}
                />
              )}
            </Match>
            <Match when={asNote(row())}>
              {(note) => (
                <NoteRow
                  note={note()}
                  onRetry={(repoId, path) => props.tree.reloadDir(repoId, path)}
                />
              )}
            </Match>
          </Switch>
        )}
      </For>
    </div>
  );
}
