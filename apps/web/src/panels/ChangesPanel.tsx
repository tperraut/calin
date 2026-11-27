import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
} from "solid-js";
import type { ChangedFile } from "../gen/ChangedFile";
import type { FileDiff } from "../gen/FileDiff";
import type { RepoChanges } from "../gen/RepoChanges";
import { errorMessage } from "../lib/errors";
import { createMediaQuery, createWidth, WIDE_QUERY } from "../lib/media";
import { loadDiffLayout, saveDiffLayout, type DiffLayoutChoice } from "../lib/prefs";
import type { Tone } from "../lib/status";
import {
  createRefresher,
  diffContent,
  filesLabel,
  findFile,
  inRepoOrder,
  repoTotals,
  sameDiff,
  sessionTotals,
  statusLook,
  type DiffContent,
} from "./changes/changes";
import { CodeEditor } from "./editor/CodeEditor";
import { DiffView, type DiffControls } from "./editor/DiffView";
import type { FileTarget, PanelProps } from "./registry";

/** Width of the panel from which the list and the diff fit side by side. */
const COLUMNS_FROM = 720;

type DiffState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; diff: FileDiff; content: DiffContent }
  | { status: "error"; message: string };

const toneClass: Record<Tone, string> = {
  neutral: "text-muted",
  active: "text-tone-active",
  attention: "text-tone-attention",
  success: "text-tone-success",
  danger: "text-tone-danger",
};

const sameTarget = (a: FileTarget | undefined, b: FileTarget): boolean =>
  a !== undefined && a.sessionRepoId === b.sessionRepoId && a.path === b.path;

function Lines(props: { added: number; removed: number }) {
  return (
    <span class="shrink-0 font-mono text-xs whitespace-nowrap">
      <span aria-hidden="true">
        <span class="text-tone-success">+{props.added}</span>{" "}
        <span class="text-tone-danger">−{props.removed}</span>
      </span>
      <span class="sr-only">
        {props.added} added, {props.removed} removed
      </span>
    </span>
  );
}

/** A path with its folder set back, so the file names line up for the eye. */
function Path(props: { path: string }) {
  const cut = () => props.path.lastIndexOf("/") + 1;
  return (
    <>
      <span class="text-muted">{props.path.slice(0, cut())}</span>
      {props.path.slice(cut())}
    </>
  );
}

function FileRow(props: { file: ChangedFile; selected: boolean; onSelect: () => void }) {
  const look = () => statusLook(props.file.status);
  return (
    <li>
      <button
        type="button"
        aria-current={props.selected ? "true" : undefined}
        class={[
          "flex min-h-11 w-full items-center gap-2 px-3 py-1.5 text-left -outline-offset-2 hover:bg-sunken",
          { "bg-accent-soft": props.selected },
        ]}
        onClick={() => props.onSelect()}
      >
        <span
          class={[
            "w-4 shrink-0 text-center font-mono text-sm font-semibold",
            toneClass[look().tone],
          ]}
          title={look().label}
        >
          <span aria-hidden="true">{look().letter}</span>
          <span class="sr-only">{look().label}</span>
        </span>
        <span class="min-w-0 flex-1 font-mono text-sm wrap-break-word">
          <Show when={props.file.old_path}>
            {(old) => (
              <>
                <Path path={old()} />
                <span class="text-muted"> → </span>
              </>
            )}
          </Show>
          <Path path={props.file.path} />
        </span>
        <Show
          when={!props.file.binary}
          fallback={
            <span class="shrink-0 rounded border border-line px-1.5 text-xs text-muted">
              binary
            </span>
          }
        >
          <Lines added={props.file.added} removed={props.file.removed} />
        </Show>
      </button>
    </li>
  );
}

function RepoGroup(props: {
  repo: RepoChanges;
  open: boolean;
  selected: FileTarget | undefined;
  onToggle: () => void;
  onSelect: (target: FileTarget) => void;
}) {
  const totals = () => repoTotals(props.repo);
  const name = () => (
    <>
      <span class="truncate font-semibold">{props.repo.name}</span>
      <Show when={props.repo.branch}>
        {(branch) => <span class="truncate font-mono text-xs text-muted">{branch()}</span>}
      </Show>
    </>
  );
  return (
    <section aria-label={props.repo.name} class="border-b border-line">
      <Show
        when={totals().files > 0}
        fallback={
          // One line, so five quiet repos do not bury the one that changed.
          <p class="flex min-h-11 items-center gap-2 px-3 text-sm">
            {name()}
            <span class="ml-auto shrink-0 text-xs text-muted">No changes</span>
          </p>
        }
      >
        <h3>
          <button
            type="button"
            aria-expanded={props.open ? "true" : "false"}
            class="flex min-h-11 w-full items-center gap-2 bg-surface px-3 text-left text-sm -outline-offset-2"
            onClick={() => props.onToggle()}
          >
            <span
              aria-hidden="true"
              class={["shrink-0 text-muted transition-transform", { "rotate-90": props.open }]}
            >
              ▸
            </span>
            {name()}
            <span class="ml-auto flex shrink-0 items-center gap-2">
              <Lines added={totals().added} removed={totals().removed} />
              <span class="text-xs text-muted">{filesLabel(totals().files)}</span>
            </span>
          </button>
        </h3>
        <ul hidden={!props.open} class="divide-y divide-line border-t border-line">
          <For each={props.repo.files} keyed={(file) => file.path}>
            {(file) => (
              <FileRow
                file={file()}
                selected={sameTarget(props.selected, {
                  sessionRepoId: props.repo.session_repo_id,
                  path: file().path,
                })}
                onSelect={() =>
                  props.onSelect({ sessionRepoId: props.repo.session_repo_id, path: file().path })
                }
              />
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

function Notice(props: { text: string }) {
  return <p class="p-6 text-center text-sm text-muted">{props.text}</p>;
}

/**
 * What the session changed, measured against each repo's base and grouped by
 * repo, with the diff of the selected file. The list is the server's (`GET
 * /changes`); `file_changed` events only say when to ask again.
 */
export default function ChangesPanel(props: PanelProps) {
  const wide = createMediaQuery(WIDE_QUERY);
  let root: HTMLDivElement | undefined;
  const width = createWidth(() => root);

  const [changes, setChanges] = createSignal<readonly RepoChanges[]>();
  const [failure, setFailure] = createSignal<string>();
  const [refreshing, setRefreshing] = createSignal(false);
  const [selected, setSelected] = createSignal<FileTarget>();
  const [diff, setDiff] = createSignal<DiffState>({ status: "idle" });
  const [closed, setClosed] = createSignal<ReadonlySet<string>>(new Set());
  const [layoutChoice, setLayoutChoice] = createSignal<DiffLayoutChoice | undefined>(
    loadDiffLayout(),
  );
  const [controls, setControls] = createSignal<DiffControls>();

  // Signals show their new value a tick later; what the requests decide on is kept in plain variables.
  let selection: FileTarget | undefined;
  let shown: FileDiff | undefined;
  let diffRequest = 0;

  /** `quiet`: a refresh of the diff on screen, which stays there unless its content changed. */
  async function loadDiff(target: FileTarget, quiet: boolean): Promise<void> {
    diffRequest += 1;
    const request = diffRequest;
    if (!quiet) setDiff({ status: "loading" });
    try {
      const loaded = await props.store.workspace.diff(target.sessionRepoId, target.path);
      if (request !== diffRequest) return;
      if (shown && sameDiff(shown, loaded)) return;
      shown = loaded;
      setDiff({ status: "ready", diff: loaded, content: diffContent(loaded) });
    } catch (error) {
      if (request !== diffRequest) return;
      shown = undefined;
      setDiff({ status: "error", message: errorMessage(error) });
    }
  }

  async function refresh(): Promise<void> {
    setRefreshing(true);
    try {
      const loaded = await props.store.workspace.changes();
      setChanges(loaded);
      setFailure(undefined);
      // The file on screen may be one of those that changed.
      if (selection && findFile(loaded, selection)) void loadDiff(selection, true);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setRefreshing(false);
    }
  }

  const refresher = createRefresher(refresh);
  // Nothing is fetched for a panel behind another one, nor for a page in a background tab.
  let active = false;
  const onScreen = (): boolean => active && document.visibilityState === "visible";

  createEffect(
    () => props.active,
    (next) => {
      active = next;
      refresher.setVisible(onScreen());
    },
  );
  onSettled(() => {
    const onVisibility = (): void => refresher.setVisible(onScreen());
    document.addEventListener("visibilitychange", onVisibility);
    refresher.now();
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      refresher.dispose();
    };
  });
  createEffect(
    () => props.store.view().changesRevision,
    () => refresher.request(),
    { defer: true },
  );
  createEffect(
    () => props.store.status(),
    (status, previous) => {
      // The first status is the session loading, not something that happened to its files.
      if (status !== undefined && previous !== undefined) refresher.request();
    },
    { defer: true },
  );

  function select(target: FileTarget): void {
    selection = target;
    shown = undefined;
    setSelected(target);
    void loadDiff(target, false);
  }

  function deselect(): void {
    selection = undefined;
    shown = undefined;
    diffRequest += 1;
    setSelected(undefined);
    setDiff({ status: "idle" });
  }

  function toggle(repoId: string): void {
    setClosed((ids) => {
      const next = new Set(ids);
      if (!next.delete(repoId)) next.add(repoId);
      return next;
    });
  }

  const ordered = createMemo(() => inRepoOrder(changes() ?? [], props.store.repos()));
  const totals = createMemo(() => sessionTotals(changes() ?? []));
  const selectedFile = createMemo(() => {
    const target = selected();
    const list = changes();
    return target && list ? findFile(list, target) : undefined;
  });
  /** The selected file left the changes (reverted, or committed away) since it was picked. */
  const gone = (): boolean =>
    selected() !== undefined && changes() !== undefined && selectedFile() === undefined;

  const mode = (): "levels" | "stacked" | "columns" =>
    !wide() ? "levels" : width() >= COLUMNS_FROM ? "columns" : "stacked";
  const layout = (): DiffLayoutChoice => (wide() ? (layoutChoice() ?? "split") : "unified");
  const ready = () => {
    const state = diff();
    return state.status === "ready" ? state : undefined;
  };
  const failed = () => {
    const state = diff();
    return state.status === "error" ? state : undefined;
  };
  const both = () => {
    const content = ready()?.content;
    return content?.kind === "both" ? content : undefined;
  };
  const single = () => {
    const content = ready()?.content;
    return content?.kind === "single" ? content : undefined;
  };

  const summary = (): string => {
    if (changes() === undefined) return failure() === undefined ? "Loading changes…" : "";
    const total = totals();
    if (total.files === 0) return "No changes yet";
    const where =
      ordered().length > 1 ? ` in ${total.changedRepos} of ${ordered().length} repos` : "";
    return `${filesLabel(total.files)} changed${where}`;
  };

  return (
    <div
      ref={(element) => (root = element)}
      class={["flex min-h-0 min-w-0 flex-1", mode() === "columns" ? "flex-row" : "flex-col"]}
    >
      <section
        aria-label="Changed files"
        hidden={mode() === "levels" && selected() !== undefined}
        class={[
          "flex min-h-0 min-w-0 flex-col",
          mode() === "columns"
            ? "w-80 shrink-0 border-r border-line"
            : selected() === undefined
              ? "flex-1"
              : "max-h-[40%] shrink-0 border-b border-line",
        ]}
      >
        <div class="flex items-center gap-2 border-b border-line bg-surface pl-3">
          <p class="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 text-sm" role="status">
            <span class="font-medium">{summary()}</span>
            <Show when={totals().files > 0}>
              <Lines added={totals().added} removed={totals().removed} />
            </Show>
          </p>
          <button
            type="button"
            class="btn border-transparent px-0"
            aria-label="Refresh the changes"
            title="Refresh the changes"
            disabled={refreshing()}
            onClick={() => refresher.now()}
          >
            <span aria-hidden="true" class={{ pulse: refreshing() }}>
              ↻
            </span>
          </button>
        </div>
        <div class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
          <Show when={failure()}>
            {(message) => (
              <div role="alert" class="flex flex-col items-start gap-2 p-3 text-sm">
                <p>The changes could not be loaded: {message()}</p>
                <button type="button" class="btn" onClick={() => refresher.now()}>
                  Try again
                </button>
              </div>
            )}
          </Show>
          <Show when={changes() !== undefined && totals().files === 0}>
            <div class="p-6 text-center">
              <p class="text-sm font-medium">No changes yet</p>
              <p class="mt-1 text-sm text-muted">
                Files the session changes in its repos show up here, with their diff.
              </p>
            </div>
          </Show>
          <Show when={totals().files > 0}>
            <For each={ordered()} keyed={(repo) => repo.session_repo_id}>
              {(repo) => (
                <RepoGroup
                  repo={repo()}
                  open={!closed().has(repo().session_repo_id)}
                  selected={selected()}
                  onToggle={() => toggle(repo().session_repo_id)}
                  onSelect={select}
                />
              )}
            </For>
          </Show>
        </div>
      </section>

      <Show when={selected()}>
        {(target) => (
          <section aria-label="Diff" class="flex min-h-0 min-w-0 flex-1 flex-col">
            <div class="flex flex-wrap items-center gap-x-1 border-b border-line bg-surface px-1">
              <Show when={mode() === "levels"}>
                <button type="button" class="btn border-transparent px-2" onClick={deselect}>
                  <span aria-hidden="true">‹</span> Changes
                </button>
              </Show>
              <p
                class="min-w-0 flex-1 basis-40 truncate px-2 py-2 font-mono text-sm"
                title={target().path}
              >
                {target().path}
              </p>
              <div class="ml-auto flex items-center gap-1">
                <Show when={both() && controls()}>
                  {(diffControls) => (
                    <>
                      <button
                        type="button"
                        class="btn border-transparent px-0"
                        aria-label="Previous change"
                        title="Previous change"
                        disabled={diffControls().changes === 0}
                        onClick={() => diffControls().previous()}
                      >
                        <span aria-hidden="true">↑</span>
                      </button>
                      <button
                        type="button"
                        class="btn border-transparent px-0"
                        aria-label="Next change"
                        title="Next change"
                        disabled={diffControls().changes === 0}
                        onClick={() => diffControls().next()}
                      >
                        <span aria-hidden="true">↓</span>
                      </button>
                    </>
                  )}
                </Show>
                <Show when={wide() && both()}>
                  <button
                    type="button"
                    class="btn px-2 text-sm"
                    aria-pressed={layout() === "split" ? "true" : "false"}
                    onClick={() => {
                      const next = layout() === "split" ? "unified" : "split";
                      setLayoutChoice(next);
                      saveDiffLayout(next);
                    }}
                  >
                    Side by side
                  </button>
                </Show>
                <button
                  type="button"
                  class="btn px-2 text-sm"
                  disabled={selectedFile() === undefined || selectedFile()?.status === "deleted"}
                  onClick={() => props.links.openFile(target())}
                >
                  Open in Files
                </button>
              </div>
            </div>
            <Switch>
              <Match when={gone()}>
                <div role="status" class="flex flex-col items-center gap-3 p-6 text-center">
                  <p class="text-sm">
                    <span class="font-mono">{target().path}</span> is no longer among the changes.
                  </p>
                  <button type="button" class="btn" onClick={deselect}>
                    Back to the changes
                  </button>
                </div>
              </Match>
              <Match when={diff().status === "loading"}>
                <Notice text="Loading the diff…" />
              </Match>
              <Match when={failed()}>
                {(state) => (
                  <div
                    role="alert"
                    class="flex flex-col items-center gap-3 p-6 text-center text-sm"
                  >
                    <p>The diff could not be loaded: {state().message}</p>
                    <button type="button" class="btn" onClick={() => select(target())}>
                      Try again
                    </button>
                  </div>
                )}
              </Match>
              <Match when={ready()?.content.kind === "binary"}>
                <Notice text="Binary file: there is no text to compare." />
              </Match>
              <Match when={ready()?.content.kind === "too_large"}>
                <Notice text="This file is too large to show as a diff." />
              </Match>
              <Match when={ready()?.content.kind === "empty"}>
                <Notice text="There is no content to compare for this file." />
              </Match>
              <Match when={single()} keyed>
                {(one) => (
                  <>
                    <p class="border-b border-line bg-sunken px-3 py-1 text-xs text-muted">
                      {one.side === "added"
                        ? "New file: all of it was added."
                        : "Deleted file: this is what it contained."}
                    </p>
                    <CodeEditor
                      label={`${target().path}, ${one.side}`}
                      path={target().path}
                      text={() => one.text}
                      revision={0}
                      readOnly={true}
                      wrap={!wide()}
                    />
                  </>
                )}
              </Match>
              <Match when={both()}>
                {(sides) => (
                  <DiffView
                    label={target().path}
                    path={target().path}
                    original={sides().original}
                    modified={sides().modified}
                    layout={layout()}
                    controls={setControls}
                  />
                )}
              </Match>
            </Switch>
          </section>
        )}
      </Show>
    </div>
  );
}
