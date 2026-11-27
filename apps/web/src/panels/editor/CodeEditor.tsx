import { createEffect, createSignal, onSettled, Show } from "solid-js";
import { errorMessage } from "../../lib/errors";
import { loadEditorModule } from "./load";
import type { EditorHandle } from "./types";

/** What the surrounding toolbar can ask of an editor once it exists. */
export type EditorControls = { openSearch(): void; focus(): void };

/**
 * One file in CodeMirror. The editor owns the text while it is open: `text` is
 * read when it is created and again each time `revision` moves (a reload from
 * disk), never on the reader's own keystrokes.
 */
export function CodeEditor(props: {
  label: string;
  path: string;
  text: () => string;
  revision: number;
  readOnly: boolean;
  wrap: boolean;
  onChange?: (text: string) => void;
  onSave?: () => void;
  controls?: (controls: EditorControls | undefined) => void;
}) {
  const [failure, setFailure] = createSignal<string>();
  const [ready, setReady] = createSignal(false);
  let host: HTMLDivElement | undefined;
  let handle: EditorHandle | undefined;

  onSettled(() => {
    let disposed = false;
    loadEditorModule().then(
      (module) => {
        if (disposed || !host) return;
        handle = module.createEditor(host, {
          label: props.label,
          path: props.path,
          text: props.text(),
          readOnly: props.readOnly,
          wrap: props.wrap,
          onChange: (text) => props.onChange?.(text),
          onSave: () => props.onSave?.(),
        });
        const editor = handle;
        props.controls?.({ openSearch: () => editor.openSearch(), focus: () => editor.focus() });
        setReady(true);
      },
      (error: unknown) => {
        if (!disposed) setFailure(errorMessage(error));
      },
    );
    return () => {
      disposed = true;
      props.controls?.(undefined);
      handle?.destroy();
      handle = undefined;
    };
  });

  createEffect(
    // The text is read here, where reads belong, each time the revision moves.
    () => ({ revision: props.revision, text: props.text() }),
    ({ text }) => handle?.setText(text),
    { defer: true },
  );
  createEffect(
    () => props.readOnly,
    (readOnly) => handle?.setReadOnly(readOnly),
    { defer: true },
  );
  createEffect(
    () => props.wrap,
    (wrap) => handle?.setWrap(wrap),
    { defer: true },
  );

  return (
    <div class="relative flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class="p-3 text-sm">
            The editor could not be loaded: {message()}
          </p>
        )}
      </Show>
      <Show when={!ready() && failure() === undefined}>
        <p class="p-3 text-sm text-muted">Loading the editor…</p>
      </Show>
      <div ref={(element) => (host = element)} class="min-h-0 min-w-0 flex-1 overflow-hidden" />
    </div>
  );
}
