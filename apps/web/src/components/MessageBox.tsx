import { createSignal, createUniqueId, Show } from "solid-js";
import { errorMessage } from "../lib/errors";
import { isSubmitChord } from "./Composer";

/**
 * The box that sends text to a session's lead agent. When `enabled` is false
 * the field shows `closedHint` instead of accepting input.
 */
export function MessageBox(props: {
  enabled: boolean;
  closedHint: string;
  onSend: (text: string) => Promise<void>;
}) {
  const [draft, setDraft] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const fieldId = createUniqueId();
  const errorId = createUniqueId();

  async function submit(): Promise<void> {
    const text = draft().trim();
    if (text === "" || !props.enabled || busy()) return;
    setBusy(true);
    setError(undefined);
    try {
      await props.onSend(text);
      setDraft("");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      class="flex flex-col gap-1.5 border-t border-line bg-surface p-3"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <Show when={error()}>
        {(message) => (
          <p id={errorId} role="alert" class="text-sm text-tone-danger">
            {message()}
          </p>
        )}
      </Show>
      <div class="flex items-end gap-2">
        <label for={fieldId} class="sr-only">
          Message to the agent
        </label>
        <textarea
          id={fieldId}
          class="field min-h-11 flex-1 resize-none px-3 py-2"
          rows="2"
          placeholder={props.enabled ? "Message the agent" : props.closedHint}
          value={draft()}
          disabled={!props.enabled}
          aria-describedby={error() === undefined ? undefined : errorId}
          onInput={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (!isSubmitChord(event)) return;
            event.preventDefault();
            void submit();
          }}
        />
        <button
          type="submit"
          class="btn btn-primary"
          title="Send (Ctrl+Enter)"
          aria-keyshortcuts="Control+Enter Meta+Enter"
          disabled={!props.enabled || busy()}
        >
          {busy() ? "Sending…" : "Send"}
        </button>
      </div>
    </form>
  );
}
