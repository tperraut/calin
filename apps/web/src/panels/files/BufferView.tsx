import { createSignal, For, Match, Show, Switch } from "solid-js";
import { compactCount } from "../../lib/format";
import { CodeEditor, type EditorControls } from "../editor/CodeEditor";
import { DiffView } from "../editor/DiffView";
import type { DiffLayout } from "../editor/types";
import type { Buffer, BufferLoad, BufferNotice, Buffers } from "./buffers";

type Action = { label: string; run: () => void; primary?: boolean };

type NoticeProps = { tone: "attention" | "danger"; text: string; actions: Action[] };

function NoticeBar(props: NoticeProps) {
  return (
    <div
      role={props.tone === "danger" ? "alert" : "status"}
      class={[
        "flex flex-wrap items-center gap-2 border-b border-line px-3 py-2 text-sm",
        props.tone === "danger" ? "bg-danger-soft" : "bg-attention-soft",
      ]}
    >
      <p class="min-w-0 flex-1 basis-48">{props.text}</p>
      <div class="flex flex-wrap gap-2">
        <For each={props.actions}>
          {(action) => (
            <button
              type="button"
              class={["btn px-3 text-sm", { "btn-primary": action.primary === true }]}
              onClick={() => action.run()}
            >
              {action.label}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}

const sizeLabel = (bytes: number): string => `${compactCount(bytes)}B`;

/** What a notice can offer the reader to do. */
type NoticeActions = Record<"reload" | "overwrite" | "compare" | "dismiss", Action>;

/** How a notice reads, and what it offers. */
function describeNotice(notice: BufferNotice, readOnly: boolean, act: NoticeActions): NoticeProps {
  switch (notice.kind) {
    case "conflict":
      return {
        tone: "danger",
        text: `Not saved, your text is kept: ${notice.message}`,
        actions: [act.reload, act.overwrite, act.compare],
      };
    case "changed":
      return {
        tone: "attention",
        text: "This file changed on disk. Your unsaved edits are kept.",
        actions: [act.reload, act.compare, act.dismiss],
      };
    case "deleted":
      return {
        tone: "attention",
        // Nothing is ever saved into a repo attached `read`, so nothing is promised there.
        text: readOnly
          ? "This file was deleted on disk."
          : "This file was deleted on disk. Saving creates it again.",
        actions: [act.dismiss],
      };
    default:
      return { tone: "danger", text: notice.message, actions: [act.dismiss] };
  }
}

/** Why there is no text to show, for a file that is not text. */
function unshowableText(load: BufferLoad): string | undefined {
  if (load.status === "binary") {
    return `Binary file (${sizeLabel(load.size)}): there is no text to show.`;
  }
  if (load.status === "too_large") {
    return `This file is too large to open here (${sizeLabel(load.size)}).`;
  }
  return undefined;
}

const failure = (load: BufferLoad) => (load.status === "error" ? load : undefined);

type ViewProps = {
  buffer: Buffer;
  buffers: Buffers;
  readOnly: boolean;
};

/** The file's path, and what can be done to its text. */
function Toolbar(
  props: ViewProps & {
    repoName: string;
    wrap: boolean;
    onWrap: (wrap: boolean) => void;
    controls: EditorControls | undefined;
  },
) {
  const busy = () => props.buffer.busy();
  return (
    <div class="flex items-center gap-1 border-b border-line bg-surface pl-3">
      <p
        class="min-w-0 flex-1 truncate font-mono text-xs text-muted"
        title={`${props.repoName}/${props.buffer.target.path}`}
      >
        {props.repoName}/{props.buffer.target.path}
      </p>
      <Show when={props.buffer.load().status === "text"}>
        <button
          type="button"
          class="btn btn-ghost px-2 text-sm"
          aria-pressed={props.wrap ? "true" : "false"}
          title="Wrap long lines"
          onClick={() => props.onWrap(!props.wrap)}
        >
          Wrap
        </button>
        <button
          type="button"
          class="btn btn-ghost px-2 text-sm"
          disabled={props.controls === undefined || props.buffer.comparing() !== undefined}
          onClick={() => props.controls?.openSearch()}
        >
          Find
        </button>
        <button
          type="button"
          class="btn btn-primary rounded-none px-3 text-sm"
          disabled={props.readOnly || !props.buffer.dirty() || busy() !== false}
          title={props.readOnly ? "This repo is attached read-only" : "Save (Ctrl+S)"}
          onClick={() => void props.buffers.save(props.buffer.key)}
        >
          {busy() === "saving" ? "Saving…" : "Save"}
        </button>
      </Show>
    </div>
  );
}

/** What shows instead of the editor while there is no text: loading, failed, or not text. */
function LoadState(props: { buffer: Buffer; buffers: Buffers }) {
  return (
    <Switch>
      <Match when={props.buffer.load().status === "loading"}>
        <p class="p-6 text-center text-sm text-muted">Loading the file…</p>
      </Match>
      <Match when={failure(props.buffer.load())}>
        {(failed) => (
          <div role="alert" class="flex flex-col items-center gap-3 p-6 text-center text-sm">
            <p>This file could not be opened: {failed().message}</p>
            <button
              type="button"
              class="btn"
              onClick={() => void props.buffers.reload(props.buffer.key)}
            >
              Try again
            </button>
          </div>
        )}
      </Match>
      <Match when={unshowableText(props.buffer.load())}>
        {(text) => <p class="p-6 text-center text-sm text-muted">{text()}</p>}
      </Match>
    </Switch>
  );
}

/** The version on disk against the reader's text. */
function Comparison(props: { buffer: Buffer; buffers: Buffers; disk: string; layout: DiffLayout }) {
  return (
    <>
      <div class="flex flex-wrap items-center gap-2 border-b border-line bg-sunken px-3 py-1">
        <p class="min-w-0 flex-1 basis-48 text-xs text-muted">
          {props.layout === "split"
            ? "Left: the file on disk. Right: your text."
            : "Removed lines are on disk only; added lines are yours."}
        </p>
        <button
          type="button"
          class="btn px-3 text-sm"
          onClick={() => props.buffers.endCompare(props.buffer.key)}
        >
          Back to editing
        </button>
      </div>
      <DiffView
        label={`${props.buffer.name}: on disk against yours`}
        path={props.buffer.target.path}
        original={props.disk}
        modified={props.buffer.text()}
        layout={props.layout}
      />
    </>
  );
}

/**
 * One open file: its toolbar, what stands between it and the disk, and the
 * editor. It stays mounted while its tab is open, so the text, the undo
 * history and the scroll position survive a switch to another tab or panel.
 */
export function BufferView(props: {
  buffer: Buffer;
  buffers: Buffers;
  shown: boolean;
  /** The repo is attached `read`: nothing in it can be saved. */
  readOnly: boolean;
  repoName: string;
  wrap: boolean;
  compareLayout: DiffLayout;
  onWrap: (wrap: boolean) => void;
  /** The reader asked to close this tab while it has unsaved text. */
  closing: boolean;
  onCloseAnswer: (discard: boolean) => void;
}) {
  const [controls, setControls] = createSignal<EditorControls>();
  const key = () => props.buffer.key;

  const actions: NoticeActions = {
    reload: { label: "Reload from disk", run: () => void props.buffers.reload(key()) },
    overwrite: { label: "Overwrite", run: () => void props.buffers.overwrite(key()) },
    compare: { label: "Compare", run: () => void props.buffers.compare(key()) },
    dismiss: { label: "Dismiss", run: () => props.buffers.dismiss(key()) },
  };
  const notice = () => {
    const current = props.buffer.notice();
    return current ? describeNotice(current, props.readOnly, actions) : undefined;
  };

  return (
    <div hidden={!props.shown} class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Toolbar
        buffer={props.buffer}
        buffers={props.buffers}
        readOnly={props.readOnly}
        repoName={props.repoName}
        wrap={props.wrap}
        onWrap={props.onWrap}
        controls={controls()}
      />
      <Show when={props.readOnly}>
        <p class="border-b border-line bg-sunken px-3 py-1.5 text-xs text-muted">
          {props.repoName} is attached read-only: this file can be read here, not saved.
        </p>
      </Show>
      <Show when={props.closing}>
        <NoticeBar
          tone="attention"
          text={`${props.buffer.name} has unsaved changes. Close it and lose them?`}
          actions={[
            { label: "Discard changes", run: () => props.onCloseAnswer(true) },
            { label: "Keep editing", run: () => props.onCloseAnswer(false), primary: true },
          ]}
        />
      </Show>
      <Show when={notice()}>
        {(bar) => <NoticeBar tone={bar().tone} text={bar().text} actions={bar().actions} />}
      </Show>
      <LoadState buffer={props.buffer} buffers={props.buffers} />
      <Show when={props.buffer.load().status === "text"}>
        <Show when={props.buffer.comparing()}>
          {(disk) => (
            <Comparison
              buffer={props.buffer}
              buffers={props.buffers}
              disk={disk()}
              layout={props.compareLayout}
            />
          )}
        </Show>
        {/* Hidden, not removed, while comparing: the editor is where the reader's text lives. */}
        <div
          hidden={props.buffer.comparing() !== undefined}
          class="flex min-h-0 min-w-0 flex-1 flex-col"
        >
          <CodeEditor
            label={props.buffer.target.path}
            path={props.buffer.target.path}
            text={() => props.buffer.text()}
            revision={props.buffer.revision()}
            readOnly={props.readOnly}
            wrap={props.wrap}
            onChange={(text) => props.buffers.edit(key(), text)}
            onSave={() => void props.buffers.save(key())}
            controls={setControls}
          />
        </div>
      </Show>
    </div>
  );
}
