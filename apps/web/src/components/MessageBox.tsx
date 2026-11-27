import { createEffect, createSignal, createUniqueId, Show } from "solid-js";
import { errorMessage } from "../lib/errors";
import { isSubmitChord } from "./Composer";

/**
 * The box that sends text to a session's lead agent. When `enabled` is false
 * the field shows `closedHint` instead of accepting input. With `resumes`, the
 * message restarts a stopped or failed agent, and the button says so.
 *
 * `prefill` replaces the draft with a text written elsewhere on the page and
 * moves the focus to it, as soon as the box is on screen; the reader sends it.
 */
export function MessageBox(props: {
  enabled: boolean;
  closedHint: string;
  resumes?: boolean;
  /** `seq` tells apart two requests for the same text. */
  prefill?: { text: string; seq: number };
  /** False while another panel covers the box; taken as true when absent. */
  shown?: boolean;
  onSend: (text: string) => Promise<void>;
}) {
  const [draft, setDraft] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const fieldId = createUniqueId();
  const errorId = createUniqueId();
  // Two lines as a rule; a longer draft (one written for the reader, say) shows more of itself.
  const rows = (): number => {
    const lines = draft().split("\n").length;
    return lines <= 2 ? 2 : Math.min(6, lines + 2);
  };
  let field: HTMLTextAreaElement | undefined;

  let applied = 0;
  let focusOwed = false;
  createEffect(
    () => ({ prefill: props.prefill, shown: props.shown !== false }),
    ({ prefill, shown }) => {
      if (prefill && prefill.seq !== applied) {
        applied = prefill.seq;
        focusOwed = true;
        setDraft(prefill.text);
      }
      // A box behind another panel cannot take the focus: it is given once the box shows.
      if (!focusOwed || !shown || !field) return;
      focusOwed = false;
      field.focus();
    },
  );

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
          ref={(element) => (field = element)}
          class="field min-h-11 flex-1 resize-none px-3 py-2"
          rows={rows()}
          placeholder={
            !props.enabled
              ? props.closedHint
              : props.resumes === true
                ? "Message the agent to continue"
                : "Message the agent"
          }
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
          title={props.resumes === true ? "Continue (Ctrl+Enter)" : "Send (Ctrl+Enter)"}
          aria-keyshortcuts="Control+Enter Meta+Enter"
          disabled={!props.enabled || busy()}
        >
          {busy() ? "Sending…" : props.resumes === true ? "Continue" : "Send"}
        </button>
      </div>
    </form>
  );
}
