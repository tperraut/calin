import { createEffect, createSignal, createUniqueId, Show, type Setter } from "solid-js";
import { createRequest } from "../lib/request";
import { onFormSubmit, onSubmitChord } from "../lib/submit";

type Prefill = { text: string; seq: number };

type MessageBoxProps = {
  enabled: boolean;
  closedHint: string;
  resumes?: boolean;
  /** `seq` tells apart two requests for the same text. */
  prefill?: Prefill;
  /** False while another panel covers the box; taken as true when absent. */
  shown?: boolean;
  onSend: (text: string) => Promise<void>;
};

/** Two lines as a rule; a longer draft (one written for the reader, say) shows more of itself. */
function draftRows(draft: string): number {
  const lines = draft.split("\n").length;
  return lines <= 2 ? 2 : Math.min(6, lines + 2);
}

function placeholder(props: MessageBoxProps): string {
  if (!props.enabled) return props.closedHint;
  return props.resumes === true ? "Message the agent to continue" : "Message the agent";
}

const verb = (props: MessageBoxProps): string => (props.resumes === true ? "Continue" : "Send");

/**
 * Puts each prefill in the draft once, and moves the focus to the field.
 * A box behind another panel cannot take the focus: it is given once the box shows.
 */
function followPrefill(
  props: MessageBoxProps,
  setDraft: Setter<string>,
  field: () => HTMLTextAreaElement | undefined,
): void {
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
      const target = field();
      if (!focusOwed || !shown || !target) return;
      focusOwed = false;
      target.focus();
    },
  );
}

/**
 * The box that sends text to a session's lead agent. When `enabled` is false
 * the field shows `closedHint` instead of accepting input. With `resumes`, the
 * message restarts a stopped or failed agent, and the button says so.
 *
 * `prefill` replaces the draft with a text written elsewhere on the page and
 * moves the focus to it, as soon as the box is on screen; the reader sends it.
 */
export function MessageBox(props: MessageBoxProps) {
  const [draft, setDraft] = createSignal("");
  const fieldId = createUniqueId();
  const errorId = createUniqueId();
  let field: HTMLTextAreaElement | undefined;
  followPrefill(props, setDraft, () => field);

  const sending = createRequest(async () => {
    await props.onSend(draft().trim());
    setDraft("");
  });
  async function submit(): Promise<void> {
    if (draft().trim() === "" || !props.enabled) return;
    await sending.run();
  }

  return (
    <form
      class="flex flex-col gap-1.5 border-t border-line bg-surface p-3"
      onSubmit={onFormSubmit(submit)}
    >
      <Show when={sending.error()}>
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
          rows={draftRows(draft())}
          placeholder={placeholder(props)}
          value={draft()}
          disabled={!props.enabled}
          aria-describedby={sending.error() === undefined ? undefined : errorId}
          onInput={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={onSubmitChord(submit)}
        />
        <button
          type="submit"
          class="btn btn-primary"
          title={`${verb(props)} (Ctrl+Enter)`}
          aria-keyshortcuts="Control+Enter Meta+Enter"
          disabled={!props.enabled || sending.busy()}
        >
          {sending.busy() ? "Sending…" : verb(props)}
        </button>
      </div>
    </form>
  );
}
