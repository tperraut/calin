import {
  createEffect,
  createSignal,
  createUniqueId,
  For,
  onSettled,
  Show,
  untrack,
  type Accessor,
  type Setter,
} from "solid-js";
import type { SessionRepo } from "../gen/SessionRepo";
import { errorMessage } from "../lib/errors";
import { createMediaQuery, createWidth, WIDE_QUERY } from "../lib/media";
import { loadEditorWrap, saveEditorWrap } from "../lib/prefs";
import type { FileChange } from "../state/sessionReducer";
import { BufferView } from "./files/BufferView";
import { createBuffers, sessionTabStorage, type Buffer, type Buffers } from "./files/buffers";
import { FileTreeView } from "./files/FileTreeView";
import { createFileTree, dirKey, type FileTree } from "./files/tree";
import type { FileTarget, PanelLinks, PanelProps } from "./registry";
import { ErrorNote } from "../components/ErrorNote";

/** Width of the panel from which the tree and the editor fit side by side. */
const COLUMNS_FROM = 600;

type NewFile = { repo: SessionRepo; dir: string };

type View = "tree" | "editor";

/** The file named `name` in the folder the form was opened for; nothing while the name is blank. */
function newFileTarget(where: NewFile | undefined, name: string): FileTarget | undefined {
  const trimmed = name.trim();
  if (!where || trimmed === "") return undefined;
  return {
    sessionRepoId: where.repo.id,
    path: where.dir === "" ? trimmed : `${where.dir}/${trimmed}`,
  };
}

/** Tells the buffers about the `file_changed` events that are not in `known`. */
function reportChanges(
  buffers: Buffers,
  files: readonly FileChange[],
  known: ReadonlyMap<string, FileChange>,
): void {
  for (const file of files) {
    if (file.sessionRepoId === null || known.get(file.key) === file) continue;
    void buffers.changedOnDisk(
      { sessionRepoId: file.sessionRepoId, path: file.path },
      file.op === "delete",
    );
  }
}

/** Unsaved text is lost with the page: while there is some, the browser asks before a reload or a close. */
function warnWhileUnsaved(dirty: boolean, links: PanelLinks): (() => void) | undefined {
  links.setUnsavedWork(dirty ? "A file has unsaved changes." : undefined);
  if (!dirty) return undefined;
  const warn = (event: BeforeUnloadEvent): void => {
    event.preventDefault();
    // Still what some browsers look at.
    event.returnValue = true;
  };
  window.addEventListener("beforeunload", warn);
  return () => window.removeEventListener("beforeunload", warn);
}

/**
 * What the panel decides apart from its markup: which side shows on a narrow
 * pane, the tab asked to close, the wrap choice and the new-file form.
 */
// A class rather than a closure of functions: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class FilesState {
  readonly view: Accessor<View>;
  readonly setView: Setter<View>;
  readonly closing: Accessor<string | undefined>;
  readonly newFile: Accessor<NewFile | undefined>;
  readonly newName: Accessor<string>;
  readonly setNewName: Setter<string>;
  readonly newError: Accessor<string | undefined>;
  readonly creating: Accessor<boolean>;
  readonly nameId: string;
  nameField: HTMLInputElement | undefined;
  private readonly wrapChoice: Accessor<boolean | undefined>;
  private readonly setWrapChoice: Setter<boolean | undefined>;
  private readonly setClosing: Setter<string | undefined>;
  private readonly setNewFile: Setter<NewFile | undefined>;
  private readonly setNewError: Setter<string | undefined>;
  private readonly setCreating: Setter<boolean>;
  private readonly buffers: Buffers;
  private readonly tree: FileTree;

  constructor(buffers: Buffers, tree: FileTree) {
    this.buffers = buffers;
    this.tree = tree;
    // Restored tabs come back on the editor; an empty panel starts on the tree.
    [this.view, this.setView] = createSignal<View>(
      untrack(() => buffers.list().length) > 0 ? "editor" : "tree",
    );
    [this.wrapChoice, this.setWrapChoice] = createSignal(loadEditorWrap());
    [this.closing, this.setClosing] = createSignal<string>();
    [this.newFile, this.setNewFile] = createSignal<NewFile>();
    [this.newName, this.setNewName] = createSignal("");
    [this.newError, this.setNewError] = createSignal<string>();
    [this.creating, this.setCreating] = createSignal(false);
    this.nameId = createUniqueId();
  }

  /** The reader's choice, else wrapped on a narrow screen. */
  wrap(wide: boolean): boolean {
    return this.wrapChoice() ?? !wide;
  }

  setWrap(wrap: boolean): void {
    this.setWrapChoice(wrap);
    saveEditorWrap(wrap);
  }

  open(target: FileTarget): void {
    this.buffers.open(target);
    this.setView("editor");
  }

  requestClose(buffer: Buffer): void {
    if (!buffer.dirty()) {
      this.buffers.close(buffer.key);
      return;
    }
    // The question is asked in the tab itself, so the text at stake is in view.
    this.buffers.activate(buffer.key);
    this.setClosing(buffer.key);
  }

  answerClose(key: string, discard: boolean): void {
    this.setClosing(undefined);
    if (discard) this.buffers.close(key);
  }

  askNewFile(repo: SessionRepo, dir: string): void {
    this.setNewFile({ repo, dir });
    this.setNewName("");
    this.setNewError(undefined);
    queueMicrotask(() => this.nameField?.focus());
  }

  cancelNewFile(): void {
    this.setNewFile(undefined);
  }

  async createFile(): Promise<void> {
    const where = this.newFile();
    const target = newFileTarget(where, this.newName());
    if (!where || !target || this.creating()) return;
    this.setCreating(true);
    this.setNewError(undefined);
    try {
      await this.buffers.create(target);
      this.tree.reloadDir(where.repo.id, where.dir);
      this.setNewFile(undefined);
      this.setView("editor");
    } catch (error) {
      // 409: a file with this name is already there (api.rs); the server says so itself.
      this.setNewError(errorMessage(error));
    } finally {
      this.setCreating(false);
    }
  }
}

function NewFileForm(props: { state: FilesState; where: NewFile }) {
  return (
    <form
      aria-label="New file"
      class="flex flex-col gap-2 border-b border-line bg-attention-soft p-3"
      onSubmit={(event) => {
        event.preventDefault();
        void props.state.createFile();
      }}
    >
      <label for={props.state.nameId} class="text-sm wrap-break-word">
        New file in{" "}
        <span class="font-mono">
          {props.where.repo.name}/{props.where.dir === "" ? "" : `${props.where.dir}/`}
        </span>
      </label>
      <input
        id={props.state.nameId}
        ref={(element) => (props.state.nameField = element)}
        type="text"
        class="field h-11 px-3 font-mono"
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
        placeholder="name.ext"
        value={props.state.newName()}
        disabled={props.state.creating()}
        onInput={(event) => props.state.setNewName(event.currentTarget.value)}
      />
      <ErrorNote message={props.state.newError()} />
      <div class="flex justify-end gap-2">
        <button
          type="button"
          class="btn"
          disabled={props.state.creating()}
          onClick={() => props.state.cancelNewFile()}
        >
          Cancel
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          disabled={props.state.creating() || props.state.newName().trim() === ""}
        >
          {props.state.creating() ? "Creating…" : "Create"}
        </button>
      </div>
    </form>
  );
}

function TreePane(props: {
  state: FilesState;
  columns: boolean;
  tree: FileTree;
  buffers: Buffers;
  repos: readonly SessionRepo[];
}) {
  const count = () => props.buffers.list().length;
  return (
    <section
      aria-label="File tree"
      hidden={!props.columns && props.state.view() === "editor"}
      class={[
        "flex min-h-0 min-w-0 flex-col",
        props.columns ? "w-72 shrink-0 border-r border-line" : "flex-1",
      ]}
    >
      <Show when={!props.columns && count() > 0}>
        <div class="flex items-center justify-between border-b border-line bg-surface pl-3">
          <span class="text-sm text-muted">
            {count()} open {count() === 1 ? "file" : "files"}
          </span>
          <button
            type="button"
            class="btn btn-ghost px-3 text-sm"
            onClick={() => props.state.setView("editor")}
          >
            Editor <span aria-hidden="true">›</span>
          </button>
        </div>
      </Show>
      <Show when={props.state.newFile()}>
        {(where) => <NewFileForm state={props.state} where={where()} />}
      </Show>
      <Show
        when={props.repos.length > 0}
        fallback={
          <p class="p-6 text-center text-sm text-muted">
            No repo is attached to this session, so there are no files to browse.
          </p>
        }
      >
        <FileTreeView
          tree={props.tree}
          repos={props.repos}
          openKey={props.buffers.activeKey()}
          onOpen={(target) => props.state.open(target)}
          onNewFile={(repo, dir) => props.state.askNewFile(repo, dir)}
        />
      </Show>
    </section>
  );
}

function Tab(props: { buffer: Buffer; active: boolean; buffers: Buffers; state: FilesState }) {
  return (
    <li
      class={[
        "flex shrink-0 items-stretch border-r border-line",
        props.active ? "bg-bg" : "text-muted",
      ]}
    >
      <button
        type="button"
        aria-current={props.active ? "true" : undefined}
        class="flex min-h-11 max-w-48 items-center gap-1.5 pr-1 pl-3 text-sm -outline-offset-2"
        title={props.buffer.target.path}
        onClick={() => props.buffers.activate(props.buffer.key)}
      >
        <Show when={props.buffer.dirty()}>
          <span class="text-tone-attention">
            <span aria-hidden="true">●</span>
            <span class="sr-only">Unsaved changes:</span>
          </span>
        </Show>
        <span class="truncate font-mono">{props.buffer.name}</span>
      </button>
      <button
        type="button"
        class="flex min-h-11 min-w-11 items-center justify-center -outline-offset-2 hover:text-fg"
        aria-label={`Close ${props.buffer.name}`}
        title="Close"
        onClick={() => props.state.requestClose(props.buffer)}
      >
        <span aria-hidden="true">×</span>
      </button>
    </li>
  );
}

function EditorPane(props: {
  state: FilesState;
  columns: boolean;
  wide: boolean;
  buffers: Buffers;
  repoOf: (id: string) => SessionRepo | undefined;
  readOnly: (target: FileTarget) => boolean;
}) {
  const repoName = (buffer: Buffer): string =>
    props.repoOf(buffer.target.sessionRepoId)?.name ?? "repo";
  return (
    <section
      aria-label="Editor"
      hidden={!props.columns && props.state.view() === "tree"}
      class="flex min-h-0 min-w-0 flex-1 flex-col"
    >
      <div class="flex items-stretch border-b border-line bg-surface">
        <Show when={!props.columns}>
          <button
            type="button"
            class="btn shrink-0 rounded-none border-0 border-r border-line px-3 text-sm"
            onClick={() => props.state.setView("tree")}
          >
            <span aria-hidden="true">‹</span> Files
          </button>
        </Show>
        <ul
          aria-label="Open files"
          class="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:thin]"
        >
          <For each={props.buffers.list()} keyed={(buffer) => buffer.key}>
            {(buffer) => (
              <Tab
                buffer={buffer()}
                active={props.buffers.activeKey() === buffer().key}
                buffers={props.buffers}
                state={props.state}
              />
            )}
          </For>
        </ul>
      </div>
      <Show when={props.buffers.list().length === 0}>
        <p class="p-6 text-center text-sm text-muted">
          No file is open. Pick one in the tree to read or edit it.
        </p>
      </Show>
      <For each={props.buffers.list()} keyed={(buffer) => buffer.key}>
        {(buffer) => (
          <BufferView
            buffer={buffer()}
            buffers={props.buffers}
            shown={props.buffers.activeKey() === buffer().key}
            readOnly={props.readOnly(buffer().target)}
            repoName={repoName(buffer())}
            wrap={props.state.wrap(props.wide)}
            compareLayout={props.wide && props.columns ? "split" : "unified"}
            onWrap={(next) => props.state.setWrap(next)}
            closing={props.state.closing() === buffer().key}
            onCloseAnswer={(discard) => props.state.answerClose(buffer().key, discard)}
          />
        )}
      </For>
    </section>
  );
}

/**
 * The session's repos as a tree, and the files opened from it as tabs of an
 * editor. Saving sends the hash a file was loaded with, so an edit the agent
 * made meanwhile is never overwritten without the reader choosing to.
 */
export default function FilesPanel(props: PanelProps) {
  const wide = createMediaQuery(WIDE_QUERY);
  let root: HTMLDivElement | undefined;
  const width = createWidth(() => root);
  /** Both at once when the pane has the room; otherwise one of them, like two screens. */
  const columns = (): boolean => width() >= COLUMNS_FROM;

  const repoOf = (id: string): SessionRepo | undefined =>
    props.store.repos().find((repo) => repo.id === id);
  const readOnly = (target: FileTarget): boolean => repoOf(target.sessionRepoId)?.mode === "read";

  const tree = createFileTree({
    list: (repoId, path) => props.store.workspace.tree(repoId, path),
    repos: () => props.store.repos(),
  });
  const buffers = createBuffers({
    read: (target) => props.store.workspace.readFile(target.sessionRepoId, target.path),
    write: (target, body) =>
      props.store.workspace.writeFile(target.sessionRepoId, target.path, body),
    readOnly,
    storage: sessionTabStorage(props.store.id),
  });
  const state = new FilesState(buffers, tree);

  onSettled(() => {
    buffers.start();
    // The primary repo is where the agent works: its files are the ones looked for first.
    const primary = props.store.repos().at(0);
    if (primary) tree.expand(dirKey(primary.id, ""));
  });

  // "Open in Files" from another panel; also the one pending when this panel first mounts.
  createEffect(
    () => props.links.fileRequest(),
    (request) => {
      if (request) state.open(request.target);
    },
  );

  // `file_changed` events for the open files. Only what happens once the replay
  // has caught up counts: the replay is history, and the files were just read.
  let known = new Map<string, FileChange>();
  let caughtUp = false;
  createEffect(
    () => ({ synced: props.store.synced(), files: props.store.view().changedFiles }),
    ({ synced, files }) => {
      if (synced && caughtUp) reportChanges(buffers, files, known);
      known = new Map(files.map((file) => [file.key, file]));
      caughtUp = synced;
    },
  );

  createEffect(
    () => buffers.anyDirty(),
    (dirty) => warnWhileUnsaved(dirty, props.links),
  );
  onSettled(() => () => props.links.setUnsavedWork(undefined));

  return (
    <div
      ref={(element) => (root = element)}
      class={["flex min-h-0 min-w-0 flex-1", columns() ? "flex-row" : "flex-col"]}
    >
      <TreePane
        state={state}
        columns={columns()}
        tree={tree}
        buffers={buffers}
        repos={props.store.repos()}
      />
      <EditorPane
        state={state}
        columns={columns()}
        wide={wide()}
        buffers={buffers}
        repoOf={repoOf}
        readOnly={readOnly}
      />
    </div>
  );
}
