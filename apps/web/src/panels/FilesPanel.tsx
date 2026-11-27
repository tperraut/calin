import {
  createEffect,
  createSignal,
  createUniqueId,
  For,
  onSettled,
  Show,
  untrack,
} from "solid-js";
import type { SessionRepo } from "../gen/SessionRepo";
import { errorMessage } from "../lib/errors";
import { createMediaQuery, createWidth, WIDE_QUERY } from "../lib/media";
import { loadEditorWrap, saveEditorWrap } from "../lib/prefs";
import type { FileChange } from "../state/sessionReducer";
import { BufferView } from "./files/BufferView";
import { createBuffers, sessionTabStorage, type Buffer } from "./files/buffers";
import { FileTreeView } from "./files/FileTreeView";
import { createFileTree, dirKey } from "./files/tree";
import type { FileTarget, PanelProps } from "./registry";

/** Width of the panel from which the tree and the editor fit side by side. */
const COLUMNS_FROM = 600;

type NewFile = { repo: SessionRepo; dir: string };

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

  // Restored tabs come back on the editor; an empty panel starts on the tree.
  const [view, setView] = createSignal<"tree" | "editor">(
    untrack(() => buffers.list().length) > 0 ? "editor" : "tree",
  );
  const [wrapChoice, setWrapChoice] = createSignal(loadEditorWrap());
  const wrap = (): boolean => wrapChoice() ?? !wide();
  const [closing, setClosing] = createSignal<string>();
  const [newFile, setNewFile] = createSignal<NewFile>();
  const [newName, setNewName] = createSignal("");
  const [newError, setNewError] = createSignal<string>();
  const [creating, setCreating] = createSignal(false);
  const nameId = createUniqueId();
  let nameField: HTMLInputElement | undefined;

  onSettled(() => {
    buffers.start();
    // The primary repo is where the agent works: its files are the ones looked for first.
    const primary = props.store.repos().at(0);
    if (primary) tree.expand(dirKey(primary.id, ""));
  });

  function open(target: FileTarget): void {
    buffers.open(target);
    setView("editor");
  }

  // "Open in Files" from another panel; also the one pending when this panel first mounts.
  createEffect(
    () => props.links.fileRequest(),
    (request) => {
      if (request) open(request.target);
    },
  );

  // `file_changed` events for the open files. Only what happens once the replay
  // has caught up counts: the replay is history, and the files were just read.
  let known = new Map<string, FileChange>();
  let caughtUp = false;
  createEffect(
    () => ({ synced: props.store.synced(), files: props.store.view().changedFiles }),
    ({ synced, files }) => {
      if (synced && caughtUp) {
        for (const file of files) {
          if (file.sessionRepoId === null || known.get(file.key) === file) continue;
          void buffers.changedOnDisk(
            { sessionRepoId: file.sessionRepoId, path: file.path },
            file.op === "delete",
          );
        }
      }
      known = new Map(files.map((file) => [file.key, file]));
      caughtUp = synced;
    },
  );

  // Unsaved text is lost with the page: the browser asks before a reload or a close.
  createEffect(
    () => buffers.anyDirty(),
    (dirty) => {
      props.links.setUnsavedWork(dirty ? "A file has unsaved changes." : undefined);
      if (!dirty) return;
      const warn = (event: BeforeUnloadEvent): void => {
        event.preventDefault();
        // Still what some browsers look at.
        event.returnValue = true;
      };
      window.addEventListener("beforeunload", warn);
      return () => window.removeEventListener("beforeunload", warn);
    },
  );
  onSettled(() => () => props.links.setUnsavedWork(undefined));

  function requestClose(buffer: Buffer): void {
    if (!buffer.dirty()) {
      buffers.close(buffer.key);
      return;
    }
    // The question is asked in the tab itself, so the text at stake is in view.
    buffers.activate(buffer.key);
    setClosing(buffer.key);
  }

  function askNewFile(repo: SessionRepo, dir: string): void {
    setNewFile({ repo, dir });
    setNewName("");
    setNewError(undefined);
    queueMicrotask(() => nameField?.focus());
  }

  async function createFile(): Promise<void> {
    const where = newFile();
    const name = newName().trim();
    if (!where || name === "" || creating()) return;
    const target: FileTarget = {
      sessionRepoId: where.repo.id,
      path: where.dir === "" ? name : `${where.dir}/${name}`,
    };
    setCreating(true);
    setNewError(undefined);
    try {
      await buffers.create(target);
      tree.reloadDir(where.repo.id, where.dir);
      setNewFile(undefined);
      setView("editor");
    } catch (error) {
      // 409: a file with this name is already there (api.rs); the server says so itself.
      setNewError(errorMessage(error));
    } finally {
      setCreating(false);
    }
  }

  return (
    <div
      ref={(element) => (root = element)}
      class={["flex min-h-0 min-w-0 flex-1", columns() ? "flex-row" : "flex-col"]}
    >
      <section
        aria-label="File tree"
        hidden={!columns() && view() === "editor"}
        class={[
          "flex min-h-0 min-w-0 flex-col",
          columns() ? "w-72 shrink-0 border-r border-line" : "flex-1",
        ]}
      >
        <Show when={!columns() && buffers.list().length > 0}>
          <div class="flex items-center justify-between border-b border-line bg-surface pl-3">
            <span class="text-sm text-muted">
              {buffers.list().length} open {buffers.list().length === 1 ? "file" : "files"}
            </span>
            <button
              type="button"
              class="btn border-transparent px-3 text-sm"
              onClick={() => setView("editor")}
            >
              Editor <span aria-hidden="true">›</span>
            </button>
          </div>
        </Show>
        <Show when={newFile()}>
          {(where) => (
            <form
              aria-label="New file"
              class="flex flex-col gap-2 border-b border-line bg-attention-soft p-3"
              onSubmit={(event) => {
                event.preventDefault();
                void createFile();
              }}
            >
              <label for={nameId} class="text-sm wrap-break-word">
                New file in{" "}
                <span class="font-mono">
                  {where().repo.name}/{where().dir === "" ? "" : `${where().dir}/`}
                </span>
              </label>
              <input
                id={nameId}
                ref={(element) => (nameField = element)}
                type="text"
                class="field h-11 px-3 font-mono"
                autocomplete="off"
                autocapitalize="off"
                spellcheck={false}
                placeholder="name.ext"
                value={newName()}
                disabled={creating()}
                onInput={(event) => setNewName(event.currentTarget.value)}
              />
              <Show when={newError()}>
                {(message) => (
                  <p role="alert" class="text-sm text-tone-danger">
                    {message()}
                  </p>
                )}
              </Show>
              <div class="flex justify-end gap-2">
                <button
                  type="button"
                  class="btn"
                  disabled={creating()}
                  onClick={() => setNewFile(undefined)}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  class="btn btn-primary"
                  disabled={creating() || newName().trim() === ""}
                >
                  {creating() ? "Creating…" : "Create"}
                </button>
              </div>
            </form>
          )}
        </Show>
        <Show
          when={props.store.repos().length > 0}
          fallback={
            <p class="p-6 text-center text-sm text-muted">
              No repo is attached to this session, so there are no files to browse.
            </p>
          }
        >
          <FileTreeView
            tree={tree}
            repos={props.store.repos()}
            openKey={buffers.activeKey()}
            onOpen={open}
            onNewFile={askNewFile}
          />
        </Show>
      </section>

      <section
        aria-label="Editor"
        hidden={!columns() && view() === "tree"}
        class="flex min-h-0 min-w-0 flex-1 flex-col"
      >
        <div class="flex items-stretch border-b border-line bg-surface">
          <Show when={!columns()}>
            <button
              type="button"
              class="btn shrink-0 rounded-none border-0 border-r border-line px-3 text-sm"
              onClick={() => setView("tree")}
            >
              <span aria-hidden="true">‹</span> Files
            </button>
          </Show>
          <ul
            aria-label="Open files"
            class="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:thin]"
          >
            <For each={buffers.list()} keyed={(buffer) => buffer.key}>
              {(buffer) => (
                <li
                  class={[
                    "flex shrink-0 items-stretch border-r border-line",
                    buffers.activeKey() === buffer().key ? "bg-bg" : "text-muted",
                  ]}
                >
                  <button
                    type="button"
                    aria-current={buffers.activeKey() === buffer().key ? "true" : undefined}
                    class="flex min-h-11 max-w-48 items-center gap-1.5 pr-1 pl-3 text-sm -outline-offset-2"
                    title={buffer().target.path}
                    onClick={() => buffers.activate(buffer().key)}
                  >
                    <Show when={buffer().dirty()}>
                      <span class="text-tone-attention">
                        <span aria-hidden="true">●</span>
                        <span class="sr-only">Unsaved changes:</span>
                      </span>
                    </Show>
                    <span class="truncate font-mono">{buffer().name}</span>
                  </button>
                  <button
                    type="button"
                    class="flex min-h-11 min-w-11 items-center justify-center -outline-offset-2 hover:text-fg"
                    aria-label={`Close ${buffer().name}`}
                    title="Close"
                    onClick={() => requestClose(buffer())}
                  >
                    <span aria-hidden="true">×</span>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </div>
        <Show when={buffers.list().length === 0}>
          <p class="p-6 text-center text-sm text-muted">
            No file is open. Pick one in the tree to read or edit it.
          </p>
        </Show>
        <For each={buffers.list()} keyed={(buffer) => buffer.key}>
          {(buffer) => (
            <BufferView
              buffer={buffer()}
              buffers={buffers}
              shown={buffers.activeKey() === buffer().key}
              readOnly={readOnly(buffer().target)}
              repoName={repoOf(buffer().target.sessionRepoId)?.name ?? "repo"}
              wrap={wrap()}
              compareLayout={wide() && columns() ? "split" : "unified"}
              onWrap={(next) => {
                setWrapChoice(next);
                saveEditorWrap(next);
              }}
              closing={closing() === buffer().key}
              onCloseAnswer={(discard) => {
                setClosing(undefined);
                if (discard) buffers.close(buffer().key);
              }}
            />
          )}
        </For>
      </section>
    </div>
  );
}
