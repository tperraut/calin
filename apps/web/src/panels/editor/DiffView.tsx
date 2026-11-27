import { createEffect, createSignal, onSettled, Show } from "solid-js";
import { errorMessage } from "../../lib/errors";
import { loadEditorModule } from "./load";
import type { DiffLayout, EditorModule } from "./types";

/** What the surrounding toolbar can ask of a diff once it exists. */
export type DiffControls = { changes: number; next(): void; previous(): void };

/** Two versions of one file, read-only, side by side or inline. Rebuilt when either text or the layout changes. */
export function DiffView(props: {
  label: string;
  path: string;
  original: string;
  modified: string;
  layout: DiffLayout;
  controls?: (controls: DiffControls | undefined) => void;
}) {
  const [module, setModule] = createSignal<EditorModule>();
  const [failure, setFailure] = createSignal<string>();
  let host: HTMLDivElement | undefined;

  onSettled(() => {
    let disposed = false;
    loadEditorModule().then(
      (loaded) => {
        if (!disposed) setModule(() => loaded);
      },
      (error: unknown) => {
        if (!disposed) setFailure(errorMessage(error));
      },
    );
    return () => {
      disposed = true;
    };
  });

  createEffect(
    () => ({
      loaded: module(),
      label: props.label,
      path: props.path,
      original: props.original,
      modified: props.modified,
      layout: props.layout,
    }),
    ({ loaded, ...options }) => {
      if (!loaded || !host) return;
      const handle = loaded.createDiff(host, options);
      props.controls?.({
        changes: handle.changes(),
        next: () => handle.nextChange(),
        previous: () => handle.previousChange(),
      });
      return () => {
        props.controls?.(undefined);
        handle.destroy();
      };
    },
  );

  return (
    <div class="relative flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class="p-3 text-sm">
            The diff view could not be loaded: {message()}
          </p>
        )}
      </Show>
      <Show when={module() === undefined && failure() === undefined}>
        <p class="p-3 text-sm text-muted">Loading the diff view…</p>
      </Show>
      <div ref={(element) => (host = element)} class="min-h-0 min-w-0 flex-1 overflow-hidden" />
    </div>
  );
}
