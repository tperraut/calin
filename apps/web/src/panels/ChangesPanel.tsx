import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  type Accessor,
  type Setter,
} from "solid-js";
import { PullRequestLink } from "../components/PullRequests";
import { PullRequestSection } from "../components/PullRequestSection";
import { ShipAction } from "../components/ShipAction";
import type { ChangedFile } from "../gen/ChangedFile";
import type { FileDiff } from "../gen/FileDiff";
import type { PullRequest } from "../gen/PullRequest";
import type { RepoChanges } from "../gen/RepoChanges";
import { inRepoSetOrder } from "../lib/delivery";
import { errorMessage } from "../lib/errors";
import { createMediaQuery, createWidth, WIDE_QUERY } from "../lib/media";
import { loadDiffLayout, saveDiffLayout, type DiffLayoutChoice } from "../lib/prefs";
import type { Tone } from "../lib/status";
import {
  diffContent,
  filesLabel,
  findFile,
  repoTotals,
  sameDiff,
  sessionTotals,
  statusLook,
  type DiffContent,
  type Totals,
} from "./changes/changes";
import { CodeEditor } from "./editor/CodeEditor";
import { DiffView, type DiffControls } from "./editor/DiffView";
import type { FileTarget, PanelLinks, PanelProps } from "./registry";

/** Width of the panel from which the list and the diff fit side by side. */
const COLUMNS_FROM = 720;

type Store = PanelProps["store"];

type DiffState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; diff: FileDiff; content: DiffContent }
  | { status: "error"; message: string };

/** A phone shows one level at a time; a wider panel puts the diff under the list, or beside it. */
type Mode = "levels" | "stacked" | "columns";

const toneClass: Record<Tone, string> = {
  neutral: "text-muted",
  active: "text-tone-active",
  attention: "text-tone-attention",
  success: "text-tone-success",
  danger: "text-tone-danger",
};

const sameTarget = (a: FileTarget | undefined, b: FileTarget): boolean =>
  a !== undefined && a.sessionRepoId === b.sessionRepoId && a.path === b.path;

function panelMode(wide: boolean, width: number): Mode {
  if (!wide) return "levels";
  return width >= COLUMNS_FROM ? "columns" : "stacked";
}

function listPlacement(mode: Mode, selected: boolean): string {
  if (mode === "columns") return "w-80 shrink-0 border-r border-line";
  return selected ? "max-h-[40%] shrink-0 border-b border-line" : "flex-1";
}

/** Side by side is for a wide screen only, and the default there. */
const diffLayout = (wide: boolean, choice: DiffLayoutChoice | undefined): DiffLayoutChoice =>
  wide ? (choice ?? "split") : "unified";

function changesSummary(
  changes: readonly RepoChanges[] | undefined,
  failed: boolean,
  total: Totals & { changedRepos: number },
  repos: number,
): string {
  if (changes === undefined) return failed ? "" : "Loading changes…";
  if (total.files === 0) return "No changes yet";
  const where = repos > 1 ? ` in ${total.changedRepos} of ${repos} repos` : "";
  return `${filesLabel(total.files)} changed${where}`;
}

function fileAt(
  changes: readonly RepoChanges[] | undefined,
  target: FileTarget | undefined,
): ChangedFile | undefined {
  return target && changes ? findFile(changes, target) : undefined;
}

/** The selected file left the changes (reverted, or committed away) since it was picked. */
const leftChanges = (
  changes: readonly RepoChanges[] | undefined,
  target: FileTarget | undefined,
): boolean => target !== undefined && changes !== undefined && !findFile(changes, target);

/** A deleted file has nothing on disk to open. */
const openable = (file: ChangedFile | undefined): boolean =>
  file !== undefined && file.status !== "deleted";

function toggled(ids: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(ids);
  if (!next.delete(id)) next.add(id);
  return next;
}

/** The file picked in the list, and its diff. */
class DiffSelection {
  readonly selected: Accessor<FileTarget | undefined>;
  readonly diff: Accessor<DiffState>;
  private readonly setSelected: Setter<FileTarget | undefined>;
  private readonly setDiff: Setter<DiffState>;
  private readonly store: Store;
  // Signals show their new value a tick later; what the requests decide on is kept in plain fields.
  private selection: FileTarget | undefined;
  private shown: FileDiff | undefined;
  private request = 0;

  constructor(store: Store) {
    this.store = store;
    [this.selected, this.setSelected] = createSignal<FileTarget>();
    [this.diff, this.setDiff] = createSignal<DiffState>({ status: "idle" });
  }

  select(target: FileTarget): void {
    this.selection = target;
    this.shown = undefined;
    this.setSelected(target);
    void this.load(target, false);
  }

  deselect(): void {
    this.selection = undefined;
    this.shown = undefined;
    this.request += 1;
    this.setSelected(undefined);
    this.setDiff({ status: "idle" });
  }

  /** The changes were read again: the file on screen may be one of those that changed. */
  refresh(changes: readonly RepoChanges[] | undefined): void {
    if (changes && this.selection && findFile(changes, this.selection)) {
      void this.load(this.selection, true);
    }
  }

  content(): DiffContent | undefined {
    const state = this.diff();
    return state.status === "ready" ? state.content : undefined;
  }

  failure(): string | undefined {
    const state = this.diff();
    return state.status === "error" ? state.message : undefined;
  }

  both(): Extract<DiffContent, { kind: "both" }> | undefined {
    const content = this.content();
    return content?.kind === "both" ? content : undefined;
  }

  single(): Extract<DiffContent, { kind: "single" }> | undefined {
    const content = this.content();
    return content?.kind === "single" ? content : undefined;
  }

  /** `quiet`: a refresh of the diff on screen, which stays there unless its content changed. */
  private async load(target: FileTarget, quiet: boolean): Promise<void> {
    this.request += 1;
    const request = this.request;
    if (!quiet) this.setDiff({ status: "loading" });
    try {
      const loaded = await this.store.workspace.diff(target.sessionRepoId, target.path);
      if (request !== this.request) return;
      if (this.shown && sameDiff(this.shown, loaded)) return;
      this.shown = loaded;
      this.setDiff({ status: "ready", diff: loaded, content: diffContent(loaded) });
    } catch (error) {
      if (request !== this.request) return;
      this.shown = undefined;
      this.setDiff({ status: "error", message: errorMessage(error) });
    }
  }
}

/**
 * Nothing is fetched for a panel behind another one, nor for a page in a
 * background tab: what the events announced meanwhile is read on the way back.
 */
class ChangesWatch {
  private readonly changes: Store["changes"];
  private active = false;
  private missed = false;

  constructor(changes: Store["changes"]) {
    this.changes = changes;
  }

  setActive(active: boolean): void {
    this.active = active;
    this.catchUp();
  }

  catchUp(): void {
    if (!this.missed || !this.onScreen()) return;
    this.missed = false;
    this.changes.now();
  }

  /** The session's events say the changes moved. */
  announced(): void {
    if (this.onScreen()) this.changes.request();
    else this.missed = true;
  }

  private onScreen(): boolean {
    return this.active && document.visibilityState === "visible";
  }
}

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
  /** The repo's pull request, once it has one: the group leads to it. */
  pullRequest: PullRequest | undefined;
  open: boolean;
  selected: FileTarget | undefined;
  onToggle: () => void;
  onSelect: (target: FileTarget) => void;
}) {
  const totals = () => repoTotals(props.repo);
  // The repo name is what the reader looks for: it keeps its width, and the
  // branch takes what is left. Cut short on screen only: the whole name is the
  // element's text and its title, and shows in full while the row has the keyboard focus.
  const name = () => (
    <>
      <span data-part="repo" class="max-w-[60%] shrink-0 truncate font-semibold">
        {props.repo.name}
      </span>
      <Show when={props.repo.branch}>
        {(branch) => (
          <span
            data-part="branch"
            class="min-w-0 flex-1 truncate font-mono text-xs text-muted group-focus-visible:wrap-anywhere group-focus-visible:whitespace-normal"
            title={branch()}
          >
            {branch()}
          </span>
        )}
      </Show>
    </>
  );
  const pullRequest = () => (
    <Show when={props.pullRequest}>
      {(one) => <PullRequestLink pullRequest={one()} repo={props.repo.name} class="shrink-0" />}
    </Show>
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
            {pullRequest()}
          </p>
        }
      >
        <h3 class="flex items-center gap-1 bg-surface pr-3 text-sm">
          <button
            type="button"
            aria-expanded={props.open ? "true" : "false"}
            class="group flex min-h-11 min-w-0 flex-1 items-center gap-2 pl-3 text-left -outline-offset-2"
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
          {pullRequest()}
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

function ChangesStatus(props: { store: Store; summary: string; totals: Totals }) {
  return (
    <div class="flex items-center gap-2 border-b border-line bg-surface pl-3">
      <p class="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 text-sm" role="status">
        <span class="font-medium">{props.summary}</span>
        <Show when={props.totals.files > 0}>
          <Lines added={props.totals.added} removed={props.totals.removed} />
        </Show>
      </p>
      <button
        type="button"
        class="btn btn-ghost px-0"
        aria-label="Refresh the changes"
        title="Refresh the changes"
        disabled={props.store.changes.refreshing()}
        onClick={() => props.store.changes.now()}
      >
        <span aria-hidden="true" class={{ pulse: props.store.changes.refreshing() }}>
          ↻
        </span>
      </button>
    </div>
  );
}

function ChangesList(props: {
  store: Store;
  links: PanelLinks;
  changes: readonly RepoChanges[] | undefined;
  ordered: readonly RepoChanges[];
  totals: Totals;
  selected: FileTarget | undefined;
  onSelect: (target: FileTarget) => void;
}) {
  const [closed, setClosed] = createSignal<ReadonlySet<string>>(new Set());
  const pullRequests = createMemo(() => props.store.delivery.pullRequests());
  return (
    <div class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <Show when={props.store.changes.failure()}>
        {(message) => (
          <div role="alert" class="flex flex-col items-start gap-2 p-3 text-sm">
            <p>The changes could not be loaded: {message()}</p>
            <button type="button" class="btn" onClick={() => props.store.changes.now()}>
              Try again
            </button>
          </div>
        )}
      </Show>
      <Show when={pullRequests().length > 0}>
        <PullRequestSection
          store={props.store}
          onSendBack={(text) => props.links.draftMessage(text)}
        />
      </Show>
      <Show when={props.changes !== undefined && props.totals.files === 0}>
        <div class="p-6 text-center">
          <p class="text-sm font-medium">No changes yet</p>
          <p class="mt-1 text-sm text-muted">
            Files the session changes in its repos show up here, with their diff.
          </p>
        </div>
      </Show>
      <Show when={props.totals.files > 0}>
        <For each={props.ordered} keyed={(repo) => repo.session_repo_id}>
          {(repo) => (
            <RepoGroup
              repo={repo()}
              pullRequest={pullRequests().find(
                (one) => one.session_repo_id === repo().session_repo_id,
              )}
              open={!closed().has(repo().session_repo_id)}
              selected={props.selected}
              onToggle={() => setClosed((ids) => toggled(ids, repo().session_repo_id))}
              onSelect={props.onSelect}
            />
          )}
        </For>
      </Show>
    </div>
  );
}

function ChangeSteps(props: { controls: DiffControls }) {
  return (
    <>
      <button
        type="button"
        class="btn btn-ghost px-0"
        aria-label="Previous change"
        title="Previous change"
        disabled={props.controls.changes === 0}
        onClick={() => props.controls.previous()}
      >
        <span aria-hidden="true">↑</span>
      </button>
      <button
        type="button"
        class="btn btn-ghost px-0"
        aria-label="Next change"
        title="Next change"
        disabled={props.controls.changes === 0}
        onClick={() => props.controls.next()}
      >
        <span aria-hidden="true">↓</span>
      </button>
    </>
  );
}

function DiffToolbar(props: {
  target: FileTarget;
  mode: Mode;
  wide: boolean;
  selection: DiffSelection;
  controls: DiffControls | undefined;
  layout: DiffLayoutChoice;
  onFlipLayout: () => void;
  file: ChangedFile | undefined;
  onOpen: () => void;
}) {
  return (
    <div class="flex flex-wrap items-center gap-x-1 border-b border-line bg-surface px-1">
      <Show when={props.mode === "levels"}>
        <button type="button" class="btn btn-ghost px-2" onClick={() => props.selection.deselect()}>
          <span aria-hidden="true">‹</span> Changes
        </button>
      </Show>
      <p
        class="min-w-0 flex-1 basis-40 truncate px-2 py-2 font-mono text-sm"
        title={props.target.path}
      >
        {props.target.path}
      </p>
      <div class="ml-auto flex items-center gap-1">
        <Show when={props.selection.both() && props.controls}>
          {(controls) => <ChangeSteps controls={controls()} />}
        </Show>
        <Show when={props.wide && props.selection.both()}>
          <button
            type="button"
            class="btn px-2 text-sm"
            aria-pressed={props.layout === "split" ? "true" : "false"}
            onClick={() => props.onFlipLayout()}
          >
            Side by side
          </button>
        </Show>
        <button
          type="button"
          class="btn px-2 text-sm"
          disabled={!openable(props.file)}
          onClick={() => props.onOpen()}
        >
          Open in Files
        </button>
      </div>
    </div>
  );
}

function SingleSide(props: {
  target: FileTarget;
  side: Extract<DiffContent, { kind: "single" }>;
  wide: boolean;
}) {
  return (
    <>
      <p class="border-b border-line bg-sunken px-3 py-1 text-xs text-muted">
        {props.side.side === "added"
          ? "New file: all of it was added."
          : "Deleted file: this is what it contained."}
      </p>
      <CodeEditor
        label={`${props.target.path}, ${props.side.side}`}
        path={props.target.path}
        text={() => props.side.text}
        revision={0}
        readOnly={true}
        wrap={!props.wide}
      />
    </>
  );
}

function DiffBody(props: {
  target: FileTarget;
  selection: DiffSelection;
  gone: boolean;
  wide: boolean;
  layout: DiffLayoutChoice;
  onControls: (controls: DiffControls | undefined) => void;
}) {
  const kind = () => props.selection.content()?.kind;
  return (
    <Switch>
      <Match when={props.gone}>
        <div role="status" class="flex flex-col items-center gap-3 p-6 text-center">
          <p class="text-sm">
            <span class="font-mono">{props.target.path}</span> is no longer among the changes.
          </p>
          <button type="button" class="btn" onClick={() => props.selection.deselect()}>
            Back to the changes
          </button>
        </div>
      </Match>
      <Match when={props.selection.diff().status === "loading"}>
        <Notice text="Loading the diff…" />
      </Match>
      <Match when={props.selection.failure()}>
        {(message) => (
          <div role="alert" class="flex flex-col items-center gap-3 p-6 text-center text-sm">
            <p>The diff could not be loaded: {message()}</p>
            <button type="button" class="btn" onClick={() => props.selection.select(props.target)}>
              Try again
            </button>
          </div>
        )}
      </Match>
      <Match when={kind() === "binary"}>
        <Notice text="Binary file: there is no text to compare." />
      </Match>
      <Match when={kind() === "too_large"}>
        <Notice text="This file is too large to show as a diff." />
      </Match>
      <Match when={kind() === "empty"}>
        <Notice text="There is no content to compare for this file." />
      </Match>
      <Match when={props.selection.single()} keyed>
        {(one) => <SingleSide target={props.target} side={one} wide={props.wide} />}
      </Match>
      <Match when={props.selection.both()}>
        {(sides) => (
          <DiffView
            label={props.target.path}
            path={props.target.path}
            original={sides().original}
            modified={sides().modified}
            layout={props.layout}
            controls={props.onControls}
          />
        )}
      </Match>
    </Switch>
  );
}

/**
 * What the session changed, measured against each repo's base and grouped by
 * repo, with the diff of the selected file. The list is the server's (`GET
 * /changes`), held by the session's store; `file_changed` events only say
 * when to ask again, and only while this panel is the one on screen.
 */
export default function ChangesPanel(props: PanelProps) {
  const wide = createMediaQuery(WIDE_QUERY);
  let root: HTMLDivElement | undefined;
  const width = createWidth(() => root);

  const changes = () => props.store.changes.list();
  const selection = new DiffSelection(props.store);
  const watch = new ChangesWatch(props.store.changes);
  const [layoutChoice, setLayoutChoice] = createSignal<DiffLayoutChoice | undefined>(
    loadDiffLayout(),
  );
  const [controls, setControls] = createSignal<DiffControls>();

  createEffect(
    () => changes(),
    (loaded) => selection.refresh(loaded),
    { defer: true },
  );
  createEffect(
    () => props.active,
    (next) => watch.setActive(next),
  );
  const catchUp = (): void => watch.catchUp();
  onSettled(() => {
    document.addEventListener("visibilitychange", catchUp);
    // Opening the panel is asking what changed: a shell's edits announce nothing.
    props.store.changes.now();
    return () => document.removeEventListener("visibilitychange", catchUp);
  });
  createEffect(
    () => props.store.view().changesRevision,
    () => watch.announced(),
    { defer: true },
  );

  const listed = () => changes() ?? [];
  const ordered = createMemo(() => inRepoSetOrder(listed(), props.store.repos()));
  const totals = createMemo(() => sessionTotals(listed()));
  const selectedFile = createMemo(() => fileAt(changes(), selection.selected()));
  const mode = () => panelMode(wide(), width());
  const layout = () => diffLayout(wide(), layoutChoice());
  const summary = () =>
    changesSummary(
      changes(),
      props.store.changes.failure() !== undefined,
      totals(),
      ordered().length,
    );

  function flipLayout(): void {
    const next = layout() === "split" ? "unified" : "split";
    setLayoutChoice(next);
    saveDiffLayout(next);
  }

  return (
    <div
      ref={(element) => (root = element)}
      class={["flex min-h-0 min-w-0 flex-1", mode() === "columns" ? "flex-row" : "flex-col"]}
    >
      <section
        aria-label="Changed files"
        hidden={mode() === "levels" && selection.selected() !== undefined}
        class={[
          "flex min-h-0 min-w-0 flex-col",
          listPlacement(mode(), selection.selected() !== undefined),
        ]}
      >
        <ChangesStatus store={props.store} summary={summary()} totals={totals()} />
        {/* On a phone the header's own is in reach from every panel; one is enough there. */}
        <Show when={wide()}>
          <ShipAction
            store={props.store}
            class="border-b border-line bg-surface px-3 py-1.5"
            onOpen={() => props.links.openShipForm()}
          />
        </Show>
        <ChangesList
          store={props.store}
          links={props.links}
          changes={changes()}
          ordered={ordered()}
          totals={totals()}
          selected={selection.selected()}
          onSelect={(target) => selection.select(target)}
        />
      </section>

      <Show when={selection.selected()}>
        {(target) => (
          <section aria-label="Diff" class="flex min-h-0 min-w-0 flex-1 flex-col">
            <DiffToolbar
              target={target()}
              mode={mode()}
              wide={wide()}
              selection={selection}
              controls={controls()}
              layout={layout()}
              onFlipLayout={flipLayout}
              file={selectedFile()}
              onOpen={() => props.links.openFile(target())}
            />
            <DiffBody
              target={target()}
              selection={selection}
              gone={leftChanges(changes(), selection.selected())}
              wide={wide()}
              layout={layout()}
              onControls={setControls}
            />
          </section>
        )}
      </Show>
    </div>
  );
}
